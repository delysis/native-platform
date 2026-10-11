//! Test the production shutdown owner with a parked supervisor fixture.
//! This proves neither native model execution nor physical-network acceptance.
use super::*;
use std::{
    future::{Future, poll_fn},
    task::Poll,
};
use tokio::sync::oneshot;

#[tokio::test]
async fn cancelled_network_shutdown_retains_its_join_handle() -> Result<()> {
    let identity = Identity::generate()?;
    let network = Network::start(&identity, NetworkMode::Direct {}).await?;
    // Join the normal supervisor, then park a fixture in the same owner slot.
    // The public shutdown path below must retain it across caller abandonment.
    network.stop.cancel();
    let (release, released) = oneshot::channel();
    {
        let mut slot = network.worker.lock().await;
        if let Some(worker) = slot.as_mut() {
            worker.await.map_err(network_error)?;
        }
        *slot = Some(tokio::spawn(async move {
            let _ = released.await;
        }));
    }
    let mut abandoned = Box::pin(network.shutdown());
    poll_fn(|cx| {
        assert!(abandoned.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(abandoned);
    let retained = network.worker.lock().await.is_some();
    let _ = release.send(());
    tokio::time::timeout(Duration::from_secs(5), network.shutdown())
        .await
        .expect("network shutdown joins released supervisor")?;
    assert!(retained, "shutdown detached its unfinished supervisor");
    assert!(network.worker.lock().await.is_none());
    Ok(())
}
