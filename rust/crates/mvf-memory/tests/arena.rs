//! Arena safety: invalid handles, bounds/overflow, fan-out refcounts, bounded admission, stale
//! generations and cleanup.

use std::sync::Arc;

use mvf_memory::*;

fn arena(slots: usize, slot_size: usize) -> Arc<Arena> {
    Arena::create(&ArenaConfig {
        slot_size,
        slot_count: slots,
        dir: None,
    })
    .expect("arena")
}

#[test]
fn publish_seal_and_read_back_in_place() {
    let a = arena(2, 4096);
    let lease = a
        .publish_copy(&Descriptor::image_u8(2, 2, 1), &[1, 2, 3, 4])
        .expect("publish");
    assert_eq!(lease.payload(), &[1, 2, 3, 4]);
    assert_eq!(lease.descriptor().shape, vec![2, 2, 1]);
    assert_eq!(lease.offset() % a.slot_size() as u64, 0);
    let m = a.metrics();
    assert_eq!((m.copies_in, m.bytes_copied_in, m.in_use), (1, 4, 1));
    drop(lease);
    assert_eq!(a.in_use(), 0);
    assert_eq!(a.metrics().reclaimed, 1);
}

#[test]
fn fan_out_shares_one_slot_until_the_last_consumer_releases() {
    let a = arena(1, 4096);
    let lease = a
        .publish_copy(&Descriptor::blob(3), &[9, 9, 9])
        .expect("publish");
    let consumers: Vec<Lease> = (0..4).map(|_| lease.clone()).collect();
    assert_eq!(a.in_use(), 1, "five references, one slot");
    drop(lease);
    for (i, c) in consumers.into_iter().enumerate() {
        assert_eq!(c.payload(), &[9, 9, 9]);
        assert_eq!(a.in_use(), 1, "still referenced after {i} releases");
        drop(c);
    }
    assert_eq!(a.in_use(), 0);
}

#[test]
fn bounded_admission_refuses_instead_of_overcommitting() {
    let a = arena(2, 1024);
    let r1 = a.reserve().expect("1");
    let _r2 = a.reserve().expect("2");
    assert_eq!(a.reserve().expect_err("full"), ArenaError::Full(2));
    assert_eq!(a.metrics().admission_refusals, 1);
    drop(r1);
    assert!(a.reserve().is_ok(), "a released slot is admitted again");
}

#[test]
fn dropped_reservation_returns_its_slot() {
    let a = arena(1, 1024);
    {
        let mut r = a.reserve().expect("reserve");
        r.payload_mut()[0] = 42; // a producer that never seals (e.g. crashed) …
    }
    assert_eq!(a.in_use(), 0, "… does not leak the slot");
}

#[test]
fn invalid_offsets_are_rejected() {
    let a = arena(2, 1024);
    assert_eq!(a.slot_of(0), Ok(0));
    assert_eq!(a.slot_of(1024), Ok(1));
    assert_eq!(a.slot_of(17), Err(ArenaError::InvalidOffset(17)));
    assert_eq!(a.slot_of(2048), Err(ArenaError::InvalidOffset(2048)));
    assert_eq!(
        a.slot_of(u64::MAX),
        Err(ArenaError::InvalidOffset(u64::MAX))
    );
}

#[test]
fn stale_generation_never_resolves_to_a_recycled_slot() {
    let a = arena(1, 1024);
    let first = a.publish_copy(&Descriptor::blob(1), &[1]).expect("first");
    let old = first.handle();
    assert_eq!(a.check(old), Ok(0));
    drop(first);
    assert!(matches!(
        a.check(old),
        Err(ArenaError::StaleGeneration { .. })
    ));
    let second = a.publish_copy(&Descriptor::blob(1), &[2]).expect("second");
    assert_eq!(second.offset(), old.offset, "same slot reused …");
    assert_ne!(
        second.handle().generation,
        old.generation,
        "… at a new generation"
    );
    assert!(matches!(
        a.check(old),
        Err(ArenaError::StaleGeneration { .. })
    ));
    assert_eq!(a.check(second.handle()), Ok(0));
}

#[test]
fn oversize_and_mismatched_payloads_are_rejected() {
    let a = arena(1, 256); // 64 bytes of payload capacity
    assert!(matches!(
        a.publish_copy(&Descriptor::blob(65), &[0; 65]),
        Err(ArenaError::TooLarge {
            len: 65,
            capacity: 64
        })
    ));
    assert!(matches!(
        a.publish_copy(&Descriptor::blob(4), &[0; 3]),
        Err(ArenaError::LengthMismatch { .. })
    ));
    assert_eq!(a.in_use(), 0, "failed publishes release their slot");
}

#[test]
fn host_refuses_to_write_a_header_larger_than_its_slot() {
    // Corrupt headers written by *another process* are covered by the worker tests
    // (`foreign_offsets_and_corrupt_headers_are_rejected`); the host API cannot produce one.
    let a = arena(1, 256);
    let mut r = a.reserve().expect("reserve");
    assert!(r.write_header(&Descriptor::blob(1_000_000)).is_err());
    drop(r);
    assert_eq!(a.in_use(), 0);
}

#[test]
fn descriptor_size_arithmetic_overflow_is_caught() {
    let d = Descriptor {
        media: MediaType::Tensor,
        element: ElementType::F64,
        shape: vec![u64::MAX / 2, 4],
        epoch: 0,
    };
    assert_eq!(d.byte_len(), None);
    let mut h = [0u8; HEADER_SIZE];
    assert_eq!(d.write_header(&mut h), Err(DescriptorError::Overflow));
    let a = arena(1, 1024);
    let mut r = a.reserve().expect("reserve");
    assert!(r.write(&d, &[]).is_err());
}

#[test]
fn arena_file_is_removed_on_drop() {
    let a = arena(1, 1024);
    let path = a.path().to_path_buf();
    assert!(path.is_file());
    drop(a);
    assert!(!path.exists(), "{} leaked", path.display());
}
