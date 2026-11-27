//! v2 framing and bodies: round trips, limits and malformed input (#32).

use mvf_ipc::frame::{FrameError, Header, Kind, read_frame, write_frame};
use mvf_ipc::msg::{Execute, ResultFlags, ResultMsg};

#[test]
fn frames_and_bodies_round_trip() {
    let exec = Execute {
        input: Some(4096),
        sequence: 42,
        out: Some((1 << 23, 1 << 22)),
        deadline_us: 2_000_000,
        camera: "cognex-ü".into(),
        content_type: "image/raw".into(),
    };
    let mut wire = Vec::new();
    write_frame(&mut wire, Header::new(Kind::Execute, 7, 3), &exec.encode()).expect("write");
    let mut body = Vec::new();
    let h = read_frame(&mut wire.as_slice(), 1 << 20, &mut body).expect("read");
    assert_eq!((h.kind, h.id, h.generation), (Kind::Execute, 7, 3));
    assert_eq!(Execute::decode(&body).expect("decode"), exec);
    let source = Execute {
        input: None,
        ..exec
    };
    assert_eq!(
        Execute::decode(&source.encode()).expect("decode").input,
        None
    );

    let result = ResultMsg {
        flags: ResultFlags::FRAME,
        out: 1 << 23,
        sequence: 9,
        tail: Some(serde_json::json!({"value": {"mean": 1.5}})),
    };
    let decoded = ResultMsg::decode(&result.encode()).expect("decode");
    assert_eq!(decoded.flags, ResultFlags::FRAME | ResultFlags::JSON);
    assert_eq!(decoded.tail, result.tail);
}

#[test]
fn a_peer_that_breaks_the_framing_is_refused_not_resynchronised() {
    let mut body = Vec::new();
    let read = |bytes: &[u8], body: &mut Vec<u8>| read_frame(&mut &bytes[..], 1 << 20, body);
    assert!(matches!(read(b"", &mut body), Err(FrameError::Eof)));
    assert!(matches!(
        read(b"XX\x02\x04aaaaaaaaaaaaaaaaaaaa", &mut body),
        Err(FrameError::BadMagic(_))
    ));
    let mut h = Header::new(Kind::Result, 1, 1);
    h.length = 8 << 20;
    assert!(
        matches!(read(&h.encode(), &mut body), Err(FrameError::Oversized { length, .. }) if length == 8 << 20),
        "an oversized body is refused from the header, before any of it is read"
    );
    let mut v3 = Header::new(Kind::Result, 1, 1).encode();
    v3[2] = 3;
    assert!(matches!(read(&v3, &mut body), Err(FrameError::Version(3))));
    let mut unknown = Header::new(Kind::Result, 1, 1).encode();
    unknown[3] = 99;
    assert!(matches!(
        read(&unknown, &mut body),
        Err(FrameError::UnknownKind(99))
    ));
    let mut h = Header::new(Kind::Result, 1, 1);
    h.length = 24;
    let mut partial = h.encode().to_vec();
    partial.extend_from_slice(&[0u8; 10]);
    assert!(matches!(
        read(&partial, &mut body),
        Err(FrameError::Truncated { got: 10, want: 24 })
    ));
    assert!(matches!(
        read(&h.encode()[..12], &mut body),
        Err(FrameError::Truncated { .. })
    ));
    assert!(ResultMsg::decode(b"short").is_err());
    assert!(Execute::decode(&[0u8; 39]).is_err());
    let mut bad_tail = ResultMsg::default().encode();
    bad_tail[0] = ResultFlags::JSON as u8;
    bad_tail.extend_from_slice(b"{not json");
    assert!(ResultMsg::decode(&bad_tail).is_err());
}
