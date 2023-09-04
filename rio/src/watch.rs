use crate::event::{EventListener, RioEvent};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::time::Duration;
use winit::window::WindowId;

const POLLING_TIMEOUT: Duration = Duration::from_secs(1);

pub fn watch<
    P: AsRef<Path> + std::marker::Send + 'static,
    T: EventListener + std::marker::Send + 'static,
>(
    path: P,
    event_proxy: T,
) -> notify::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();

    // Automatically select the best implementation for your platform.
    // You can also access each implementation directly e.g. INotifyWatcher.
    let mut watcher = RecommendedWatcher::new(
        tx,
        Config::default().with_poll_interval(POLLING_TIMEOUT),
    )?;

    tokio::spawn(async move {
        // Add a path to be watched. All files and directories at that path and
        // below will be monitored for changes.
        if let Err(err_message) =
            watcher.watch(path.as_ref(), RecursiveMode::NonRecursive)
        {
            log::warn!("unable to watch config directory {err_message:?}");
        };

        for res in rx {
            match res {
                Ok(event) => match event.kind {
                    EventKind::Any
                    | EventKind::Create(_)
                    | EventKind::Modify(_)
                    | EventKind::Other => {
                        log::info!("config directory has dispatched an event {event:?}");
                        // TODO: Refactor to send_global_event
                        event_proxy.send_event(RioEvent::UpdateConfig, WindowId::from(0));
                    }
                    _ => (),
                },
                Err(err_message) => {
                    log::error!("unable to watch config directory: {err_message:?}")
                }
            }
        }
    });

    Ok(())
}