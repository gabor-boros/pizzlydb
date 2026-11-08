//! Unsigned byte order of encoded keys equals the order of the original values.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use pizzlydb_core::{
    KeyBuf, NamespaceId, NodeId, Value, decode_value, decode_value_key, encode_f64_key,
    encode_i64_key, encode_value, encode_value_key, index_cmp,
};

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn short_bytes(&mut self) -> Vec<u8> {
        const ALPHABET: [u8; 5] = [0x00, 0x01, b'a', b'b', 0xFF];
        let len = self.below(5);
        (0..len)
            .map(|_| ALPHABET[usize::try_from(self.below(5)).unwrap()])
            .collect()
    }

    fn short_string(&mut self) -> String {
        const ALPHABET: [char; 5] = ['\0', '\u{1}', 'a', 'b', 'é'];
        let len = self.below(5);
        (0..len)
            .map(|_| ALPHABET[usize::try_from(self.below(5)).unwrap()])
            .collect()
    }
}

fn value_key(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_value_key(value, &mut out).unwrap();
    out
}

#[test]
fn i64_table_sorts_numerically() {
    let numbers = [
        i64::MIN,
        i64::MIN + 1,
        -2,
        -1,
        0,
        1,
        2,
        i64::MAX - 1,
        i64::MAX,
    ];
    let encoded: Vec<[u8; 8]> = numbers.iter().map(|n| encode_i64_key(*n)).collect();
    let mut sorted = encoded.clone();
    sorted.sort();
    assert_eq!(sorted, encoded);
}

#[test]
fn f64_table_sorts_from_negative_to_positive_infinity() {
    let numbers = [
        f64::NEG_INFINITY,
        f64::MIN,
        -1.0,
        -f64::MIN_POSITIVE,
        -5e-324,
        0.0,
        5e-324,
        f64::MIN_POSITIVE,
        1.0,
        f64::MAX,
        f64::INFINITY,
    ];
    let encoded: Vec<[u8; 8]> = numbers
        .iter()
        .map(|n| encode_f64_key(*n).unwrap())
        .collect();
    for pair in encoded.windows(2) {
        assert!(pair[0] < pair[1]);
    }
    assert_eq!(encode_f64_key(-0.0), encode_f64_key(0.0));
}

#[test]
fn btree_map_of_encoded_keys_iterates_in_numeric_order() {
    let mut rng = SplitMix64(1);
    let mut ints = BTreeMap::new();
    let mut floats = BTreeMap::new();
    let mut expected_ints = Vec::new();
    let mut expected_floats = Vec::new();
    for _ in 0..500 {
        let int = rng.next().cast_signed();
        ints.insert(encode_i64_key(int), int);
        expected_ints.push(int);

        let float = f64::from_bits(rng.next());
        if float.is_nan() {
            continue;
        }
        floats.insert(encode_f64_key(float).unwrap(), float);
        expected_floats.push(float);
    }
    expected_ints.sort_unstable();
    expected_ints.dedup();
    expected_floats.sort_by(f64::total_cmp);
    expected_floats.dedup();

    assert_eq!(ints.into_values().collect::<Vec<_>>(), expected_ints);
    assert_eq!(floats.into_values().collect::<Vec<_>>(), expected_floats);
}

#[test]
fn random_pairs_compare_like_the_values() {
    let mut rng = SplitMix64(2);
    for _ in 0..5_000 {
        let (a, b) = (rng.next().cast_signed(), rng.next().cast_signed());
        assert_eq!(encode_i64_key(a).cmp(&encode_i64_key(b)), a.cmp(&b));

        let (a, b) = (f64::from_bits(rng.next()), f64::from_bits(rng.next()));
        if let Some(order) = a.partial_cmp(&b) {
            assert_eq!(
                encode_f64_key(a).unwrap().cmp(&encode_f64_key(b).unwrap()),
                order
            );
        }

        let (a, b) = (rng.short_string(), rng.short_string());
        assert_eq!(
            value_key(&Value::String(a.clone())).cmp(&value_key(&Value::String(b.clone()))),
            a.cmp(&b),
            "{a:?} vs {b:?}"
        );

        let (a, b) = (rng.short_bytes(), rng.short_bytes());
        assert_eq!(
            value_key(&Value::Bytes(a.clone())).cmp(&value_key(&Value::Bytes(b.clone()))),
            a.cmp(&b),
            "{a:?} vs {b:?}"
        );
    }
}

#[test]
fn tuples_with_a_trailing_id_sort_like_rust_tuples() {
    let mut rng = SplitMix64(3);
    let mut tuples = Vec::new();
    for _ in 0..1_000 {
        let text = rng.short_string();
        let id = NodeId::from_bytes({
            let mut bytes = [0u8; 16];
            bytes[0] = u8::try_from(rng.below(256)).unwrap();
            bytes[15] = 1;
            bytes
        })
        .unwrap();
        tuples.push((text, id));
    }

    let namespace = NamespaceId::from_u128(9).unwrap();
    let encode = |(text, id): &(String, NodeId)| {
        let mut key = KeyBuf::new(namespace, 1);
        key.push_value(&Value::String(text.clone()))
            .unwrap()
            .push_id(*id);
        key.into_vec()
    };

    for pair in tuples.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        assert_eq!(encode(a).cmp(&encode(b)), a.cmp(b), "{a:?} vs {b:?}");
    }
}

#[test]
fn null_sorts_first_and_numbers_share_one_order() {
    let key_order = [
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::Float(f64::NEG_INFINITY),
        Value::Int(i64::MIN),
        Value::Int(i64::MAX),
        Value::Float(f64::INFINITY),
        Value::from(""),
        Value::from("\u{10FFFF}"),
        Value::Bytes(vec![]),
        Value::Bytes(vec![0xFF; 4]),
    ];
    for pair in key_order.windows(2) {
        assert_eq!(
            value_key(&pair[0]).cmp(&value_key(&pair[1])),
            Ordering::Less,
            "{:?} vs {:?}",
            pair[0],
            pair[1]
        );
    }

    let cell_order = [
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::Int(i64::MIN),
        Value::Int(i64::MAX),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(f64::INFINITY),
        Value::from(""),
        Value::from("\u{10FFFF}"),
        Value::Bytes(vec![]),
        Value::Bytes(vec![0xFF; 4]),
    ];
    for pair in cell_order.windows(2) {
        assert!(
            encode_value(&pair[0]).unwrap() < encode_value(&pair[1]).unwrap(),
            "{:?} vs {:?}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn namespace_prefix_scan_returns_only_that_namespace() {
    let first = NamespaceId::from_u128(1).unwrap();
    let second = NamespaceId::from_u128(2).unwrap();
    let mut map = BTreeMap::new();
    for namespace in [first, second] {
        for kind in [0_u8, 1, 0xFF] {
            let mut key = KeyBuf::new(namespace, kind);
            key.push_bytes(&[0xFF; 8]);
            map.insert(key.into_vec(), namespace);
        }
    }

    let prefix = first.as_bytes();
    let scanned: Vec<NamespaceId> = map
        .range(prefix.to_vec()..)
        .take_while(|(key, _)| key.starts_with(prefix))
        .map(|(_, namespace)| *namespace)
        .collect();
    assert_eq!(scanned, vec![first; 3]);
}

#[test]
fn round_trips_hold_for_random_values() {
    let mut rng = SplitMix64(4);
    for _ in 0..2_000 {
        let value = match rng.below(6) {
            0 => Value::Null,
            1 => Value::Bool(rng.below(2) == 1),
            2 => Value::Int(rng.next().cast_signed()),
            3 => {
                let float = f64::from_bits(rng.next());
                if float.is_nan() {
                    continue;
                }
                Value::Float(float)
            }
            4 => Value::String(rng.short_string()),
            _ => Value::Bytes(rng.short_bytes()),
        };
        assert_eq!(
            decode_value(&encode_value(&value).unwrap()),
            Ok(value.clone())
        );
        let key = value_key(&value);
        let (decoded, used) = decode_value_key(&key).unwrap();
        assert_eq!(used, key.len());
        assert_eq!(index_cmp(&decoded, &value), Ordering::Equal);
    }
}

#[test]
fn every_truncation_of_a_cell_is_an_error() {
    let values = [
        Value::Bool(true),
        Value::Int(7),
        Value::Float(2.5),
        Value::from("abc"),
        Value::Bytes(vec![0, 1, 2]),
    ];
    for value in values {
        let cell = encode_value(&value).unwrap();
        for len in 0..cell.len() {
            assert!(decode_value(&cell[..len]).is_err(), "{value:?} at {len}");
        }
        let key = value_key(&value);
        for len in 0..key.len() {
            assert!(decode_value_key(&key[..len]).is_err(), "{value:?} at {len}");
        }
    }
}
