use crate::app::core::WindowRegistry;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn register_unregister_tracks_count_and_empty_flag() {
    let reg: WindowRegistry<u32> = WindowRegistry::default();
    let a = reg.register(1u32);
    let b = reg.register(2u32);
    assert_eq!(reg.count(), 2);
    assert!(!reg.is_empty());

    assert!(!reg.unregister(a), "still one window left");
    assert_eq!(reg.count(), 1);
    assert!(reg.unregister(b), "registry now empty");
    assert!(reg.is_empty());
}

#[test]
fn unregister_of_unknown_id_is_harmless() {
    let reg: WindowRegistry<u32> = WindowRegistry::default();
    let _ = reg.register(7u32);
    assert!(!reg.unregister(u64::MAX));
    assert_eq!(reg.count(), 1);
}

#[test]
fn for_each_visits_live_entries() {
    let reg: WindowRegistry<u32> = WindowRegistry::default();
    let _a = reg.register(10u32);
    let b = reg.register(20u32);
    reg.unregister(b);
    let seen = Rc::new(Cell::new(0u32));
    let seen2 = seen.clone();
    reg.for_each(move |h| seen2.set(seen2.get() + *h));
    assert_eq!(seen.get(), 10);
}

#[test]
fn config_listeners_fire_on_broadcast() {
    let reg: WindowRegistry<u32> = WindowRegistry::default();
    let id = reg.register(1u32);
    let hits = Rc::new(Cell::new(0));
    let h1 = hits.clone();
    reg.add_config_listener(id, Rc::new(move || h1.set(h1.get() + 1)));
    reg.broadcast_config_changed();
    reg.broadcast_config_changed();
    assert_eq!(hits.get(), 2);
}

#[test]
fn config_listener_stops_firing_after_window_unregisters() {
    let reg: WindowRegistry<u32> = WindowRegistry::default();
    let id = reg.register(1u32);
    let hits = Rc::new(Cell::new(0));
    let h1 = hits.clone();
    reg.add_config_listener(id, Rc::new(move || h1.set(h1.get() + 1)));
    reg.broadcast_config_changed();
    assert_eq!(hits.get(), 1);

    reg.unregister(id);
    reg.broadcast_config_changed();
    assert_eq!(hits.get(), 1, "closed window's listener must be pruned");
}
