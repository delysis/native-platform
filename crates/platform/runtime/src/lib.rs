#![forbid(unsafe_code)]

//! Explicit ownership root for one native-platform process.
//!
//! The runtime has no global registry. Callers inject concrete service hosts,
//! retain a single non-cloneable owner, and project cloneable `Arc` handles to
//! application components. Graceful shutdown drains admission-owning services
//! before releasing the passive service references and finally joining the
//! native model host.

use attachment_native_host::AttachmentHost;
use fte_router::{Gateway, GatewayShutdownReport};
use information_native_host::InformationHost;
use llama_native_host::{JoinedNativeHost, NativeHost};
use llama_native_types::NativeError;
use speech_native_host::{SpeechHost, SpeechHostError};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum PlatformBuildError {
    #[error("a native host is required")]
    NativeRequired,
    #[error("a gateway is required")]
    GatewayRequired,
}

#[derive(Default)]
pub struct PlatformBuilder {
    native: Option<Arc<NativeHost>>,
    gateway: Option<Arc<Gateway>>,
    attachment: Option<Arc<AttachmentHost>>,
    information: Option<Arc<InformationHost>>,
    speech: Option<Arc<SpeechHost>>,
}

impl PlatformBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_native(mut self, native: Arc<NativeHost>) -> Self {
        self.native = Some(native);
        self
    }

    #[must_use]
    pub fn with_gateway(mut self, gateway: Arc<Gateway>) -> Self {
        self.gateway = Some(gateway);
        self
    }

    #[must_use]
    pub fn with_attachment(mut self, attachment: Arc<AttachmentHost>) -> Self {
        self.attachment = Some(attachment);
        self
    }

    #[must_use]
    pub fn with_information(mut self, information: Arc<InformationHost>) -> Self {
        self.information = Some(information);
        self
    }

    #[must_use]
    pub fn with_speech(mut self, speech: Arc<SpeechHost>) -> Self {
        self.speech = Some(speech);
        self
    }

    pub fn build(self) -> Result<PlatformRuntimeOwner, PlatformBuildError> {
        Ok(PlatformRuntimeOwner {
            native: self.native.ok_or(PlatformBuildError::NativeRequired)?,
            gateway: self.gateway.ok_or(PlatformBuildError::GatewayRequired)?,
            attachment: self.attachment,
            information: self.information,
            speech: self.speech,
        })
    }
}

/// The sole orderly-shutdown authority for a composed platform runtime.
///
/// This type intentionally does not implement `Clone`. Application components
/// receive [`PlatformRuntimeHandle`] projections instead.
pub struct PlatformRuntimeOwner {
    native: Arc<NativeHost>,
    gateway: Arc<Gateway>,
    attachment: Option<Arc<AttachmentHost>>,
    information: Option<Arc<InformationHost>>,
    speech: Option<Arc<SpeechHost>>,
}

impl PlatformRuntimeOwner {
    #[must_use]
    pub fn handle(&self) -> PlatformRuntimeHandle {
        PlatformRuntimeHandle {
            native: Arc::clone(&self.native),
            gateway: Arc::clone(&self.gateway),
            attachment: self.attachment.clone(),
            information: self.information.clone(),
            speech: self.speech.clone(),
        }
    }

    /// Drains every asynchronous admission owner before the final native join.
    ///
    /// A gateway or speech error does not skip later teardown. Attachment and
    /// Information have no asynchronous owner shutdown; the owner retains its
    /// references until both admission-owning services have completed, then
    /// releases them before closing the native host.
    pub async fn shutdown(self) -> PlatformShutdownReport {
        let PlatformRuntimeOwner {
            native,
            gateway,
            attachment,
            information,
            speech,
        } = self;

        let mut completion_order = Vec::with_capacity(if speech.is_some() { 4 } else { 3 });
        let gateway = gateway.shutdown_with_report().await;
        completion_order.push(PlatformShutdownStep::Gateway);

        let speech = match speech {
            Some(speech) => {
                let result = speech.shutdown().await;
                completion_order.push(PlatformShutdownStep::Speech);
                Some(result)
            }
            None => None,
        };

        drop(information);
        drop(attachment);
        completion_order.push(PlatformShutdownStep::PassiveServicesReleased);

        let native = native.shutdown_joined();
        completion_order.push(PlatformShutdownStep::Native);

        PlatformShutdownReport {
            gateway,
            speech,
            native,
            completion_order,
        }
    }
}

#[derive(Clone)]
pub struct PlatformRuntimeHandle {
    native: Arc<NativeHost>,
    gateway: Arc<Gateway>,
    attachment: Option<Arc<AttachmentHost>>,
    information: Option<Arc<InformationHost>>,
    speech: Option<Arc<SpeechHost>>,
}

impl PlatformRuntimeHandle {
    #[must_use]
    pub fn native(&self) -> Arc<NativeHost> {
        Arc::clone(&self.native)
    }

    #[must_use]
    pub fn gateway(&self) -> Arc<Gateway> {
        Arc::clone(&self.gateway)
    }

    #[must_use]
    pub fn attachment(&self) -> Option<Arc<AttachmentHost>> {
        self.attachment.clone()
    }

    #[must_use]
    pub fn information(&self) -> Option<Arc<InformationHost>> {
        self.information.clone()
    }

    #[must_use]
    pub fn speech(&self) -> Option<Arc<SpeechHost>> {
        self.speech.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformShutdownStep {
    Gateway,
    Speech,
    PassiveServicesReleased,
    Native,
}

pub struct PlatformShutdownReport {
    pub gateway: GatewayShutdownReport,
    pub speech: Option<Result<(), SpeechHostError>>,
    pub native: Result<JoinedNativeHost, NativeError>,
    pub completion_order: Vec<PlatformShutdownStep>,
}

impl PlatformShutdownReport {
    #[must_use]
    pub fn fully_joined(&self) -> bool {
        self.gateway.result.is_ok()
            && self.speech.as_ref().is_none_or(Result::is_ok)
            && self.native.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use attachment_native_host::AttachmentHostConfig;
    use fte_router::GatewayDefaults;
    use llama_native_host::NativeHostConfig;

    fn native() -> Arc<NativeHost> {
        Arc::new(NativeHost::new(NativeHostConfig::default()))
    }

    fn gateway() -> Arc<Gateway> {
        Arc::new(Gateway::new(GatewayDefaults::default()))
    }

    #[test]
    fn build_reports_each_missing_required_service() {
        assert!(matches!(
            PlatformBuilder::new().build(),
            Err(PlatformBuildError::NativeRequired)
        ));
        assert!(matches!(
            PlatformBuilder::new().with_native(native()).build(),
            Err(PlatformBuildError::GatewayRequired)
        ));
    }

    #[test]
    fn handle_projects_the_exact_injected_hosts() {
        let native = native();
        let gateway = gateway();
        let attachment = Arc::new(
            AttachmentHost::new(AttachmentHostConfig::default())
                .expect("the default attachment policy is valid"),
        );
        let speech = Arc::new(SpeechHost::default());
        let owner = PlatformBuilder::new()
            .with_native(Arc::clone(&native))
            .with_gateway(Arc::clone(&gateway))
            .with_attachment(Arc::clone(&attachment))
            .with_speech(Arc::clone(&speech))
            .build()
            .expect("required services are present");

        let handle = owner.handle();
        assert!(Arc::ptr_eq(&handle.native(), &native));
        assert!(Arc::ptr_eq(&handle.gateway(), &gateway));
        assert!(Arc::ptr_eq(
            &handle.attachment().expect("attachment projection"),
            &attachment
        ));
        assert!(handle.information().is_none());
        assert!(Arc::ptr_eq(
            &handle.speech().expect("speech projection"),
            &speech
        ));
    }

    #[tokio::test]
    async fn shutdown_drains_admission_owners_before_the_native_join() {
        let native = native();
        let owner = PlatformBuilder::new()
            .with_native(Arc::clone(&native))
            .with_gateway(gateway())
            .with_speech(Arc::new(SpeechHost::default()))
            .build()
            .expect("required services are present");

        let report = owner.shutdown().await;
        assert!(report.fully_joined());
        assert_eq!(
            report.completion_order,
            [
                PlatformShutdownStep::Gateway,
                PlatformShutdownStep::Speech,
                PlatformShutdownStep::PassiveServicesReleased,
                PlatformShutdownStep::Native,
            ]
        );
        let joined = report.native.expect("native host joined");
        assert!(joined.belongs_to(&native));
        assert_eq!(joined.joined_worker_count(), 0);
    }
}
