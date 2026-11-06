//! Opaque 128-bit identifiers.
//!
//! Every identifier is [`ID_LEN`] bytes. The all-zero value is invalid and
//! cannot be constructed. The bytes do not encode a storage location.
//!
//! Ordering is unsigned byte order. [`NodeId::from_u128`] and the matching
//! constructors on the other types write big-endian bytes, so numeric order
//! of the `u128` equals byte order of the identifier.
//!
//! Each kind of identifier is a distinct type. A function that takes a
//! [`NodeId`] does not accept a [`LabelId`].

use std::fmt;
use std::hash::Hash;

use crate::error::CoreError;

/// Width of every identifier in bytes.
pub const ID_LEN: usize = 16;

mod sealed {
    pub trait Sealed {}
}

/// Common interface of the identifier types in this module.
///
/// The trait is sealed. Generic key builders and scans use it to write and
/// read identifier bytes without knowing the concrete type.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{EdgeId, Identifier, NodeId};
///
/// fn first_byte<I: Identifier>(id: I) -> u8 {
///     id.as_bytes()[0]
/// }
///
/// let node = NodeId::from_u128(1).unwrap();
/// let edge = <EdgeId as Identifier>::from_bytes(*node.as_bytes()).unwrap();
/// assert_eq!(first_byte(node), first_byte(edge));
/// ```
pub trait Identifier:
    Copy + Ord + Hash + fmt::Debug + fmt::Display + sealed::Sealed + 'static
{
    /// Build an identifier from its bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidIdentity`] when every byte is zero.
    fn from_bytes(bytes: [u8; ID_LEN]) -> Result<Self, CoreError>;

    /// Borrow the identifier bytes.
    fn as_bytes(&self) -> &[u8; ID_LEN];
}

const fn is_zero(bytes: &[u8; ID_LEN]) -> bool {
    let mut index = 0;
    while index < ID_LEN {
        if bytes[index] != 0 {
            return false;
        }
        index += 1;
    }
    true
}

fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8; ID_LEN]) -> fmt::Result {
    for byte in bytes {
        write!(f, "{byte:02x}")?;
    }
    Ok(())
}

macro_rules! identifier {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        ///
        /// # Examples
        ///
        #[doc = concat!(
            "```\n",
            "use pizzlydb_core::{CoreError, ", stringify!($name), "};\n\n",
            "let id = ", stringify!($name), "::from_u128(42).unwrap();\n",
            "assert_eq!(id.to_u128(), 42);\n",
            "assert_eq!(", stringify!($name), "::from_u128(0), Err(CoreError::InvalidIdentity));\n",
            "```"
        )]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; ID_LEN]);

        impl $name {
            /// Build an identifier from its bytes.
            ///
            /// # Errors
            ///
            /// Returns [`CoreError::InvalidIdentity`] when every byte is zero.
            ///
            /// # Examples
            ///
            #[doc = concat!(
                "```\n",
                "use pizzlydb_core::", stringify!($name), ";\n\n",
                "let mut bytes = [0u8; 16];\n",
                "bytes[15] = 1;\n",
                "let id = ", stringify!($name), "::from_bytes(bytes).unwrap();\n",
                "assert_eq!(id.as_bytes(), &bytes);\n",
                "assert!(", stringify!($name), "::from_bytes([0u8; 16]).is_err());\n",
                "```"
            )]
            pub const fn from_bytes(bytes: [u8; ID_LEN]) -> Result<Self, CoreError> {
                if is_zero(&bytes) {
                    Err(CoreError::InvalidIdentity)
                } else {
                    Ok(Self(bytes))
                }
            }

            /// Build an identifier from the big-endian bytes of `value`.
            ///
            /// # Errors
            ///
            /// Returns [`CoreError::InvalidIdentity`] when `value` is zero.
            ///
            /// # Examples
            ///
            #[doc = concat!(
                "```\n",
                "use pizzlydb_core::", stringify!($name), ";\n\n",
                "let low = ", stringify!($name), "::from_u128(1).unwrap();\n",
                "let high = ", stringify!($name), "::from_u128(256).unwrap();\n",
                "assert!(low < high);\n",
                "assert_eq!(high.as_bytes()[14], 1);\n",
                "```"
            )]
            pub const fn from_u128(value: u128) -> Result<Self, CoreError> {
                Self::from_bytes(value.to_be_bytes())
            }

            /// Borrow the identifier bytes.
            ///
            /// # Examples
            ///
            #[doc = concat!(
                "```\n",
                "use pizzlydb_core::", stringify!($name), ";\n\n",
                "let id = ", stringify!($name), "::from_u128(9).unwrap();\n",
                "assert_eq!(id.as_bytes()[15], 9);\n",
                "```"
            )]
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; ID_LEN] {
                &self.0
            }

            /// Copy the identifier bytes.
            ///
            /// # Examples
            ///
            #[doc = concat!(
                "```\n",
                "use pizzlydb_core::", stringify!($name), ";\n\n",
                "let id = ", stringify!($name), "::from_u128(3).unwrap();\n",
                "assert_eq!(id.to_bytes()[15], 3);\n",
                "```"
            )]
            #[must_use]
            pub const fn to_bytes(self) -> [u8; ID_LEN] {
                self.0
            }

            /// Read the identifier bytes as a big-endian `u128`.
            ///
            /// # Examples
            ///
            #[doc = concat!(
                "```\n",
                "use pizzlydb_core::", stringify!($name), ";\n\n",
                "let id = ", stringify!($name), "::from_u128(u128::MAX).unwrap();\n",
                "assert_eq!(id.to_u128(), u128::MAX);\n",
                "```"
            )]
            #[must_use]
            pub const fn to_u128(self) -> u128 {
                u128::from_be_bytes(self.0)
            }
        }

        impl sealed::Sealed for $name {}

        impl Identifier for $name {
            fn from_bytes(bytes: [u8; ID_LEN]) -> Result<Self, CoreError> {
                $name::from_bytes(bytes)
            }

            fn as_bytes(&self) -> &[u8; ID_LEN] {
                $name::as_bytes(self)
            }
        }

        impl TryFrom<[u8; ID_LEN]> for $name {
            type Error = CoreError;

            fn try_from(bytes: [u8; ID_LEN]) -> Result<Self, Self::Error> {
                Self::from_bytes(bytes)
            }
        }

        /// Shorter input is [`CoreError::Truncated`]. Longer input and the
        /// all-zero value are [`CoreError::InvalidIdentity`].
        impl TryFrom<&[u8]> for $name {
            type Error = CoreError;

            fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
                if bytes.len() < ID_LEN {
                    return Err(CoreError::Truncated);
                }
                let array = <[u8; ID_LEN]>::try_from(bytes)
                    .map_err(|_| CoreError::InvalidIdentity)?;
                Self::from_bytes(array)
            }
        }

        impl From<$name> for [u8; ID_LEN] {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "("))?;
                write_hex(f, &self.0)?;
                f.write_str(")")
            }
        }

        /// Writes 32 lowercase hexadecimal digits.
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_hex(f, &self.0)
            }
        }
    };
}

identifier! {
    /// Tenant-scoped graph namespace. Every stored key starts with one.
    NamespaceId
}

identifier! {
    /// Node identifier.
    NodeId
}

identifier! {
    /// Edge identifier.
    EdgeId
}

identifier! {
    /// Node label identifier.
    LabelId
}

identifier! {
    /// Relationship type identifier.
    TypeId
}

identifier! {
    /// Property key identifier.
    PropertyKeyId
}

identifier! {
    /// Secondary index identifier.
    IndexId
}

#[cfg(test)]
mod tests {
    use super::{EdgeId, ID_LEN, Identifier, NodeId};
    use crate::error::CoreError;

    #[test]
    fn zero_is_rejected_by_every_constructor() {
        assert_eq!(
            NodeId::from_bytes([0; ID_LEN]),
            Err(CoreError::InvalidIdentity)
        );
        assert_eq!(NodeId::from_u128(0), Err(CoreError::InvalidIdentity));
        assert_eq!(
            NodeId::try_from([0u8; ID_LEN]),
            Err(CoreError::InvalidIdentity)
        );
        assert_eq!(
            NodeId::try_from(&[0u8; ID_LEN][..]),
            Err(CoreError::InvalidIdentity)
        );
        assert_eq!(
            <EdgeId as Identifier>::from_bytes([0; ID_LEN]),
            Err(CoreError::InvalidIdentity)
        );
    }

    #[test]
    fn slice_width_is_checked() {
        assert_eq!(NodeId::try_from(&[1u8; 15][..]), Err(CoreError::Truncated));
        assert_eq!(
            NodeId::try_from(&[1u8; 17][..]),
            Err(CoreError::InvalidIdentity)
        );
        assert!(NodeId::try_from(&[1u8; 16][..]).is_ok());
    }

    #[test]
    fn byte_order_matches_numeric_order() {
        let values = [1_u128, 2, 255, 256, 65_536, u128::from(u64::MAX), u128::MAX];
        for pair in values.windows(2) {
            let low = NodeId::from_u128(pair[0]).unwrap();
            let high = NodeId::from_u128(pair[1]).unwrap();
            assert!(low < high);
            assert!(low.as_bytes() < high.as_bytes());
        }
    }

    #[test]
    fn round_trip_and_formatting() {
        let id = NodeId::from_u128(0xab).unwrap();
        assert_eq!(NodeId::from_bytes(id.to_bytes()), Ok(id));
        assert_eq!(<[u8; ID_LEN]>::from(id), id.to_bytes());
        assert_eq!(id.to_string(), format!("{:032x}", 0xab));
        assert_eq!(format!("{id:?}"), format!("NodeId({:032x})", 0xab));
    }
}
