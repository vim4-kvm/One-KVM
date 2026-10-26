use byteorder::{BigEndian, ByteOrder};
#[cfg(any(test, not(feature = "openssl-hmac")))]
use hmac::{Hmac, Mac};
#[cfg(any(test, not(feature = "openssl-hmac")))]
use sha1::Sha1;

use super::{Cipher, Kdf};
use crate::error::{Error, Result};
use crate::key_derivation::*;
use crate::protection_profile::*;

#[cfg(not(feature = "openssl"))]
mod ctrcipher;

#[cfg(feature = "openssl")]
mod opensslcipher;

#[cfg(not(feature = "openssl"))]
pub(crate) use ctrcipher::CipherAesCmHmacSha1;

#[cfg(feature = "openssl")]
pub(crate) use opensslcipher::CipherAesCmHmacSha1;

#[cfg(not(feature = "openssl-hmac"))]
type HmacSha1 = Hmac<Sha1>;

// Keep the keyed SHA1 states, as RustCrypto does, without allocating an EVP
// context for every packet. Only the digest backend changes; SRTP stays HMAC-SHA1.
#[cfg(feature = "openssl-hmac")]
#[derive(Clone)]
struct HmacSha1 {
    inner: openssl::sha::Sha1,
    outer: openssl::sha::Sha1,
}

#[cfg(feature = "openssl-hmac")]
impl HmacSha1 {
    fn new_from_slice(key: &[u8]) -> std::result::Result<Self, hmac::digest::InvalidLength> {
        let mut padded = [0; 64];
        if key.len() > padded.len() {
            padded[..20].copy_from_slice(&openssl::sha::sha1(key));
        } else {
            padded[..key.len()].copy_from_slice(key);
        }
        let mut ipad = padded;
        let mut opad = padded;
        for byte in &mut ipad {
            *byte ^= 0x36;
        }
        for byte in &mut opad {
            *byte ^= 0x5c;
        }
        let mut inner = openssl::sha::Sha1::new();
        let mut outer = openssl::sha::Sha1::new();
        inner.update(&ipad);
        outer.update(&opad);
        Ok(Self { inner, outer })
    }

    fn update(&mut self, bytes: &[u8]) {
        self.inner.update(bytes);
    }
}

fn finalize_hmac(signer: HmacSha1) -> [u8; 20] {
    #[cfg(feature = "openssl-hmac")]
    {
        let mut outer = signer.outer;
        outer.update(&signer.inner.finish());
        outer.finish()
    }
    #[cfg(not(feature = "openssl-hmac"))]
    {
        signer.finalize().into_bytes().into()
    }
}

pub const CIPHER_AES_CM_HMAC_SHA1AUTH_TAG_LEN: usize = 10;

pub(crate) struct CipherInner {
    profile: ProtectionProfile,
    srtp_session_salt: Vec<u8>,
    srtp_session_auth: HmacSha1,
    srtcp_session_salt: Vec<u8>,
    srtcp_session_auth: HmacSha1,
}

impl CipherInner {
    pub fn new(
        profile: ProtectionProfile,
        kdf: Kdf,
        master_key: &[u8],
        master_salt: &[u8],
    ) -> Result<Self> {
        let srtp_session_salt = kdf(
            LABEL_SRTP_SALT,
            master_key,
            master_salt,
            0,
            master_salt.len(),
        )?;
        let srtcp_session_salt = kdf(
            LABEL_SRTCP_SALT,
            master_key,
            master_salt,
            0,
            master_salt.len(),
        )?;

        let auth_key_len = profile.auth_key_len();
        let srtp_session_auth_tag = kdf(
            LABEL_SRTP_AUTHENTICATION_TAG,
            master_key,
            master_salt,
            0,
            auth_key_len,
        )?;
        let srtcp_session_auth_tag = kdf(
            LABEL_SRTCP_AUTHENTICATION_TAG,
            master_key,
            master_salt,
            0,
            auth_key_len,
        )?;

        let srtp_session_auth = HmacSha1::new_from_slice(&srtp_session_auth_tag)
            .map_err(|e| Error::Other(e.to_string()))?;
        let srtcp_session_auth = HmacSha1::new_from_slice(&srtcp_session_auth_tag)
            .map_err(|e| Error::Other(e.to_string()))?;

        Ok(Self {
            profile,
            srtp_session_salt,
            srtp_session_auth,
            srtcp_session_salt,
            srtcp_session_auth,
        })
    }

    /// https://tools.ietf.org/html/rfc3711#section-4.2
    /// In the case of SRTP, M SHALL consist of the Authenticated
    /// Portion of the packet (as specified in Figure 1) concatenated with
    /// the roc, M = Authenticated Portion || roc;
    ///
    /// The pre-defined authentication transform for SRTP is HMAC-SHA1
    /// [RFC2104].  With HMAC-SHA1, the SRTP_PREFIX_LENGTH (Figure 3) SHALL
    /// be 0.  For SRTP (respectively SRTCP), the HMAC SHALL be applied to
    /// the session authentication key and M as specified above, i.e.,
    /// HMAC(k_a, M).  The HMAC output SHALL then be truncated to the n_tag
    /// left-most bits.
    /// - Authenticated portion of the packet is everything BEFORE MKI
    /// - k_a is the session message authentication key
    /// - n_tag is the bit-length of the output authentication tag
    fn generate_srtp_auth_tag(&self, buf: &[u8], roc: u32) -> [u8; 20] {
        let mut signer = self.srtp_session_auth.clone();

        signer.update(buf);

        // For SRTP only, we need to hash the rollover counter as well.
        signer.update(&roc.to_be_bytes());

        finalize_hmac(signer)
    }

    /// https://tools.ietf.org/html/rfc3711#section-4.2
    ///
    /// The pre-defined authentication transform for SRTP is HMAC-SHA1
    /// [RFC2104].  With HMAC-SHA1, the SRTP_PREFIX_LENGTH (Figure 3) SHALL
    /// be 0.  For SRTP (respectively SRTCP), the HMAC SHALL be applied to
    /// the session authentication key and M as specified above, i.e.,
    /// HMAC(k_a, M).  The HMAC output SHALL then be truncated to the n_tag
    /// left-most bits.
    /// - Authenticated portion of the packet is everything BEFORE MKI
    /// - k_a is the session message authentication key
    /// - n_tag is the bit-length of the output authentication tag
    fn generate_srtcp_auth_tag(&self, buf: &[u8]) -> [u8; 20] {
        let mut signer = self.srtcp_session_auth.clone();

        signer.update(buf);

        finalize_hmac(signer)
    }

    fn get_rtcp_index(&self, input: &[u8]) -> usize {
        let tail_offset = input.len() - (self.profile.rtcp_auth_tag_len() + SRTCP_INDEX_SIZE);
        (BigEndian::read_u32(&input[tail_offset..tail_offset + SRTCP_INDEX_SIZE]) & !(1 << 31))
            as usize
    }
}

#[cfg(all(test, feature = "openssl-hmac"))]
mod openssl_hmac_tests {
    use super::*;

    fn reference(key: &[u8], chunks: &[&[u8]]) -> [u8; 20] {
        let mut signer = Hmac::<Sha1>::new_from_slice(key).unwrap();
        for chunk in chunks {
            signer.update(chunk);
        }
        signer.finalize().into_bytes().into()
    }

    #[test]
    fn openssl_hmac_matches_rfc2202() {
        for (key, message, expected) in [
            (
                vec![0x0b; 20],
                b"Hi There".as_slice(),
                [
                    0xb6, 0x17, 0x31, 0x86, 0x55, 0x05, 0x72, 0x64, 0xe2, 0x8b, 0xc0, 0xb6, 0xfb,
                    0x37, 0x8c, 0x8e, 0xf1, 0x46, 0xbe, 0x00,
                ],
            ),
            (
                vec![0xaa; 80],
                b"Test Using Larger Than Block-Size Key - Hash Key First".as_slice(),
                [
                    0xaa, 0x4a, 0xe5, 0xe1, 0x52, 0x72, 0xd0, 0x0e, 0x95, 0x70, 0x56, 0x37, 0xce,
                    0x8a, 0x3b, 0x55, 0xed, 0x40, 0x21, 0x12,
                ],
            ),
        ] {
            let mut signer = HmacSha1::new_from_slice(&key).unwrap();
            signer.update(message);
            assert_eq!(finalize_hmac(signer), expected);
        }
    }

    #[test]
    fn openssl_auth_tags_match_rustcrypto_for_rtp_and_srtcp() {
        for key_len in [0, 20, 63, 64, 65, 80] {
            let key: Vec<u8> = (0..key_len).map(|i| (i * 7) as u8).collect();
            let cipher = CipherInner {
                profile: ProtectionProfile::Aes128CmHmacSha1_80,
                srtp_session_salt: vec![],
                srtp_session_auth: HmacSha1::new_from_slice(&key).unwrap(),
                srtcp_session_salt: vec![],
                srtcp_session_auth: HmacSha1::new_from_slice(&key).unwrap(),
            };
            for size in [0, 12, 55, 56, 63, 64, 65, 128, 512, 1200, 1400] {
                let packet: Vec<u8> = (0..size).map(|i| (i * 13) as u8).collect();
                for index in [0, 1, 65535, 0x7fff_ffff, u32::MAX] {
                    let roc = index.to_be_bytes();
                    let expected = reference(&key, &[&packet, &roc]);
                    // Reusing the saved keyed state must not retain a prior packet.
                    for _ in 0..2 {
                        assert_eq!(cipher.generate_srtp_auth_tag(&packet, index), expected);
                    }
                    for e_bit in [0, 1 << 31] {
                        let mut srtcp = packet.clone();
                        srtcp.extend_from_slice(&((index & 0x7fff_ffff) | e_bit).to_be_bytes());
                        assert_eq!(
                            cipher.generate_srtcp_auth_tag(&srtcp),
                            reference(&key, &[&srtcp])
                        );
                    }
                }
            }
        }
    }
}
