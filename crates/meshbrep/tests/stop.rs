//! `Options::should_stop`: a caller can abandon a reconstruction, and a
//! stop signal that never fires changes nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use meshbrep::primitives::{self, Transform};
use meshbrep::{Error, Options, reconstruct};

#[test]
fn a_stop_signal_stops_reconstruction() {
    let mesh = primitives::frustum(10.0, 5.0, 3.0, 32, &Transform::IDENTITY);
    let opts = Options {
        should_stop: Some(Arc::new(|| true)),
        ..Options::default()
    };
    assert_eq!(reconstruct(&mesh, &opts), Err(Error::Stopped));
}

#[test]
fn a_stop_signal_that_never_fires_changes_nothing() {
    let mesh = primitives::frustum(10.0, 5.0, 3.0, 32, &Transform::IDENTITY);
    let polls = Arc::new(AtomicUsize::new(0));
    let counted = polls.clone();
    let opts = Options {
        should_stop: Some(Arc::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            false
        })),
        ..Options::default()
    };
    let with = reconstruct(&mesh, &opts).expect("reconstructs");
    let without = reconstruct(&mesh, &Options::default()).expect("reconstructs");
    assert_eq!(with, without);
    // Between stages and once per face: more than one poll for a
    // three-face solid.
    assert!(polls.load(Ordering::Relaxed) > 3);
}

#[test]
fn a_stop_part_way_through_stops_too() {
    // The signal fires on its fifth poll, after reconstruction began.
    let mesh = primitives::sphere(4.0, 32, &Transform::IDENTITY);
    let polls = Arc::new(AtomicUsize::new(0));
    let counted = polls.clone();
    let opts = Options {
        should_stop: Some(Arc::new(move || {
            counted.fetch_add(1, Ordering::Relaxed) >= 4
        })),
        ..Options::default()
    };
    assert_eq!(reconstruct(&mesh, &opts), Err(Error::Stopped));
}
