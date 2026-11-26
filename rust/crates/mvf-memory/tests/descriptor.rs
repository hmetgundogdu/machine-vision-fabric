//! Descriptor header: exact byte layout (golden, from `PayloadDescriptor.cs`) and every rejection.

use mvf_memory::*;

/// Header for an Image/UInt8 [2,3,3] descriptor at epoch 7, as `PayloadDescriptor.WriteHeader`
/// produces it (`PayloadDescriptorTests.WriteThenRead_RoundTripsShapeTypeAndLength`).
fn golden() -> [u8; HEADER_SIZE] {
    let mut h = [0u8; HEADER_SIZE];
    h[0..4].copy_from_slice(&0x3146_564Du32.to_le_bytes()); // 'MVF1'
    h[4..6].copy_from_slice(&1u16.to_le_bytes()); // version
    h[6] = 1; // contiguous
    h[8..12].copy_from_slice(&7u32.to_le_bytes()); // epoch
    h[12..14].copy_from_slice(&2u16.to_le_bytes()); // Image
    h[14] = 0; // UInt8
    h[15] = 3; // rank
    h[16..24].copy_from_slice(&18i64.to_le_bytes()); // 2*3*3
    for (i, (dim, stride)) in [(2i64, 9i64), (3, 3), (3, 1)].into_iter().enumerate() {
        h[24 + i * 8..32 + i * 8].copy_from_slice(&dim.to_le_bytes());
        h[88 + i * 8..96 + i * 8].copy_from_slice(&stride.to_le_bytes());
    }
    h
}

fn image() -> Descriptor {
    Descriptor {
        media: MediaType::Image,
        element: ElementType::U8,
        shape: vec![2, 3, 3],
        epoch: 7,
    }
}

#[test]
fn write_matches_the_dotnet_layout_byte_for_byte() {
    let mut h = [0xAAu8; HEADER_SIZE];
    image().write_header(&mut h).expect("write");
    assert_eq!(h, golden());
}

#[test]
fn read_round_trips() {
    assert_eq!(Descriptor::read_header(&golden()), Ok(image()));
    let f32 = Descriptor {
        media: MediaType::Tensor,
        element: ElementType::F32,
        shape: vec![4],
        epoch: 0,
    };
    let mut h = [0u8; HEADER_SIZE];
    f32.write_header(&mut h).expect("write");
    let back = Descriptor::read_header(&h).expect("read");
    assert_eq!(back.byte_len(), Some(16));
    assert_eq!(back.strides(), Some(vec![4]));
}

#[test]
fn header_size_keeps_payload_64_byte_aligned() {
    assert_eq!(HEADER_SIZE % 64, 0);
}

type Corruption = Box<dyn Fn(&mut [u8; HEADER_SIZE])>;

#[test]
fn rejects_every_malformed_header() {
    let cases: Vec<(&str, Corruption, DescriptorError)> = vec![
        (
            "magic",
            Box::new(|h| h[0] = b'X'),
            DescriptorError::BadMagic,
        ),
        ("version", Box::new(|h| h[4] = 2), DescriptorError::BadMagic),
        (
            "contiguity",
            Box::new(|h| h[6] = 0),
            DescriptorError::NonContiguous,
        ),
        ("rank", Box::new(|h| h[15] = 9), DescriptorError::Rank(9)),
        (
            "element",
            Box::new(|h| h[14] = 200),
            DescriptorError::ElementType(200),
        ),
        (
            "media",
            Box::new(|h| h[12] = 99),
            DescriptorError::MediaType(99),
        ),
        (
            "negative dim",
            Box::new(|h| h[24..32].copy_from_slice(&(-1i64).to_le_bytes())),
            DescriptorError::NegativeDimension,
        ),
        (
            "length",
            Box::new(|h| h[16..24].copy_from_slice(&9999i64.to_le_bytes())),
            DescriptorError::LengthMismatch {
                declared: 9999,
                computed: 18,
            },
        ),
        (
            "strides",
            Box::new(|h| h[88..96].copy_from_slice(&1i64.to_le_bytes())),
            DescriptorError::Strides,
        ),
        (
            "overflow",
            Box::new(|h| {
                h[15] = 2;
                h[24..32].copy_from_slice(&i64::MAX.to_le_bytes());
                h[32..40].copy_from_slice(&i64::MAX.to_le_bytes());
            }),
            DescriptorError::Overflow,
        ),
    ];
    for (name, corrupt, expected) in cases {
        let mut h = golden();
        corrupt(&mut h);
        assert_eq!(Descriptor::read_header(&h), Err(expected), "{name}");
    }
    assert_eq!(
        Descriptor::read_header(&golden()[..100]),
        Err(DescriptorError::Truncated)
    );
}

#[test]
fn zero_strides_from_writers_that_omit_them_are_accepted() {
    let mut h = golden();
    h[88..152].fill(0);
    assert_eq!(Descriptor::read_header(&h), Ok(image()));
}
