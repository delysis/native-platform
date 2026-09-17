//! Native transport for the EASL widget's shared two-axis reveal policy.
use crate::{Error, runtime::Runtime};

pub struct Viewport {
    runtime: Runtime,
    faulted: bool,
}
impl std::fmt::Debug for Viewport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewport")
            .field("faulted", &self.faulted)
            .finish()
    }
}
impl Viewport {
    pub fn new() -> Result<Self, Error> {
        let runtime = std::panic::catch_unwind(|| {
            Runtime::compile(&[
                (
                    "viewport-native.easl",
                    include_str!("../library/viewport-native.easl"),
                ),
                ("viewport.easl", include_str!("../library/viewport.easl")),
                ("geometry.easl", include_str!("../library/geometry.easl")),
                ("atlas.easl", include_str!("../library/atlas.easl")),
            ])
        })
        .map_err(|_| Error::Language("Viewport compiler panicked".into()))??;
        Ok(Self {
            runtime,
            faulted: false,
        })
    }
    pub fn reveal(
        &mut self,
        offset: [f32; 2],
        viewport: [f32; 2],
        extent: [f32; 2],
        rect: [f32; 4],
        margin: [f32; 2],
    ) -> Result<[f32; 2], Error> {
        if self.faulted {
            return Err(Error::Faulted);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for (name, values) in [
                ("native-scroll-offset", offset.as_slice()),
                ("native-scroll-viewport", viewport.as_slice()),
                ("native-scroll-extent", extent.as_slice()),
                ("native-scroll-rect", rect.as_slice()),
                ("native-scroll-margin", margin.as_slice()),
            ] {
                if values.iter().any(|value| !value.is_finite()) {
                    return Err(Error::Invalid);
                }
                self.runtime.write(
                    name,
                    &values
                        .iter()
                        .map(|value| value.to_bits())
                        .collect::<Vec<_>>(),
                )?;
            }
            // External CPU values use packed words, independent of GPU alignment.
            self.runtime
                .write("native-scroll-result", &[u32::MAX, 0, 0])?;
            self.runtime.run("native-scroll")?;
            let result = self.runtime.read("native-scroll-result")?;
            let [0, x, y] = result.as_slice() else {
                return Err(Error::Invalid);
            };
            let result = [f32::from_bits(*x), f32::from_bits(*y)];
            if result.iter().any(|v| !v.is_finite() || *v < 0.) {
                return Err(Error::Invalid);
            }
            Ok(result)
        }));
        match result {
            Ok(result) => result,
            Err(_) => {
                self.faulted = true;
                Err(Error::Language("Viewport VM panicked".into()))
            }
        }
    }
}
