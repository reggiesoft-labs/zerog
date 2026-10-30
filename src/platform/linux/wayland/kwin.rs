use ashpd::zbus::{Connection, Proxy};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::debug;

use super::runtime::wayland_runtime_handle;

enum KWinCommand {
    SetHidden(bool),
    Shutdown,
}

pub(crate) struct KWinEffectsManager {
    cmd_tx: UnboundedSender<KWinCommand>,
}

impl KWinEffectsManager {
    pub(crate) fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        wayland_runtime_handle().spawn(run_kwin_effects_worker(rx));
        Self { cmd_tx: tx }
    }

    pub(crate) fn set_hidden(&self, hidden: bool) {
        let _ = self.cmd_tx.send(KWinCommand::SetHidden(hidden));
    }
}

impl Drop for KWinEffectsManager {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(KWinCommand::Shutdown);
    }
}

async fn run_kwin_effects_worker(mut rx: UnboundedReceiver<KWinCommand>) {
    let Ok(conn) = Connection::session().await else {
        return;
    };
    let Ok(proxy) = Proxy::new_owned(
        conn,
        "org.kde.KWin",
        "/Effects",
        "org.kde.kwin.Effects",
    )
    .await else {
        return;
    };

    let mut unloaded_by_us = false;

    while let Some(cmd) = rx.recv().await {
        match cmd {
            KWinCommand::SetHidden(true) => {
                if !unloaded_by_us {
                    let is_loaded: Result<bool, _> =
                        proxy.call("isEffectLoaded", &("shakecursor",)).await;
                    if let Ok(true) = is_loaded {
                        let unload_res: Result<(), _> =
                            proxy.call("unloadEffect", &("shakecursor",)).await;
                        if unload_res.is_ok() {
                            unloaded_by_us = true;
                            debug!("KWin shakecursor effect unloaded for remote control");
                        }
                    }
                }
            }
            KWinCommand::SetHidden(false) => {
                if unloaded_by_us {
                    let load_res: Result<bool, _> =
                        proxy.call("loadEffect", &("shakecursor",)).await;
                    if load_res.is_ok() {
                        debug!("KWin shakecursor effect reloaded for local control");
                    }
                    unloaded_by_us = false;
                }
            }
            KWinCommand::Shutdown => {
                break;
            }
        }
    }

    if unloaded_by_us {
        let _ = proxy
            .call::<&str, (&str,), bool>("loadEffect", &("shakecursor",))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kwin_effects_manager_lifecycle_does_not_panic() {
        let mgr = KWinEffectsManager::new();
        mgr.set_hidden(true);
        mgr.set_hidden(false);
        drop(mgr);
    }
}
