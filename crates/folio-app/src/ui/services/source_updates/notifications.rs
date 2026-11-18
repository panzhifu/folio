use std::sync::{Arc, Mutex, OnceLock, Weak};

use folio_core::Store;

type Subscriber = (Weak<Store>, async_channel::Sender<()>);

fn subscribers() -> &'static Mutex<Vec<Subscriber>> {
    static SUBSCRIBERS: OnceLock<Mutex<Vec<Subscriber>>> = OnceLock::new();
    SUBSCRIBERS.get_or_init(|| Mutex::new(Vec::new()))
}

pub(super) fn subscribe(store: &Arc<Store>) -> async_channel::Receiver<()> {
    let (sender, receiver) = async_channel::bounded(1);
    let mut subscribers = subscribers()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    subscribers.retain(|(store, sender)| store.strong_count() > 0 && !sender.is_closed());
    subscribers.push((Arc::downgrade(store), sender));
    receiver
}

/// 后台同步只能发数据失效信号；界面实体按同一Store身份接收。
pub(super) fn signal(store: &Arc<Store>) {
    let mut subscribers = subscribers()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    subscribers.retain(|(owner, sender)| {
        let Some(owner) = owner.upgrade() else {
            return false;
        };
        if sender.is_closed() {
            return false;
        }
        if Arc::ptr_eq(&owner, store) {
            let _ = sender.try_send(());
        }
        true
    });
}
