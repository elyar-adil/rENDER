//! Versioned, bounded payloads for the asynchronous HTTP disk cache.
//!
//! Disk entries cannot contain [`std::time::Instant`], because an instant is
//! meaningful only in the process that created it. This format stores a wall
//! clock timestamp and the response freshness duration instead. A reader can
//! conservatively reconstruct the remaining lifetime against its own clock.

use std::fmt;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use render_net::{ContentType, FetchResponse, Header, HttpStatus, RedirectResponse, Url};

const MAGIC: [u8; 8] = *b"RNPAY001";
const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const MAX_STRING_BYTES: usize = 1024 * 1024;
const MAX_HEADERS: usize = 1024;
const MAX_REDIRECTS: usize = 64;
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// A complete cacheable response plus wall-clock freshness metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskCachePayload {
    pub response: FetchResponse,
    pub stored_at: SystemTime,
    pub freshness: Duration,
}

impl DiskCachePayload {
    /// Creates a payload from a response and its cache lifetime.
    #[must_use]
    pub const fn new(response: FetchResponse, stored_at: SystemTime, freshness: Duration) -> Self {
        Self {
            response,
            stored_at,
            freshness,
        }
    }

    /// Serializes the payload into a bounded, versioned byte record.
    ///
    /// # Errors
    ///
    /// Returns [`DiskCachePayloadError::TooLarge`] for values outside the
    /// format limits, or [`DiskCachePayloadError::InvalidTime`] for a wall
    /// clock value before the Unix epoch.
    pub fn encode(&self) -> Result<Vec<u8>, DiskCachePayloadError> {
        let stored = self
            .stored_at
            .duration_since(UNIX_EPOCH)
            .map_err(|_| DiskCachePayloadError::InvalidTime)?;
        let mut writer = Writer::new();
        writer.bytes(&MAGIC)?;
        writer.u64(stored.as_secs());
        writer.u32(stored.subsec_nanos());
        writer.u64(self.freshness.as_secs());
        writer.u32(self.freshness.subsec_nanos());
        writer.url(&self.response.requested_url)?;
        writer.url(&self.response.final_url)?;
        writer.urls(&self.response.redirect_chain)?;
        writer.redirects(&self.response.redirects)?;
        writer.u16(self.response.status.as_u16());
        writer.headers(&self.response.headers)?;
        writer.content_type(self.response.content_type.as_ref())?;
        writer.bytes_limited(&self.response.body, MAX_BODY_BYTES)?;
        writer.finish()
    }

    /// Decodes and validates one disk record.
    ///
    /// # Errors
    ///
    /// Returns a typed error for truncated, malformed, oversized, or trailing
    /// data. No decoded field is trusted before its size and representation
    /// have been checked.
    pub fn decode(bytes: &[u8]) -> Result<Self, DiskCachePayloadError> {
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(DiskCachePayloadError::TooLarge);
        }
        let mut reader = Reader::new(bytes);
        if reader.bytes(MAGIC.len())? != MAGIC {
            return Err(DiskCachePayloadError::InvalidFormat);
        }
        let stored_secs = reader.u64()?;
        let stored_nanos = reader.u32()?;
        let stored_at = UNIX_EPOCH
            .checked_add(duration_parts(stored_secs, stored_nanos)?)
            .ok_or(DiskCachePayloadError::InvalidTime)?;
        let freshness = duration_parts(reader.u64()?, reader.u32()?)?;
        let requested_url = reader.url()?;
        let final_url = reader.url()?;
        let redirect_chain = reader.urls()?;
        let redirects = reader.redirects()?;
        let status = HttpStatus::from_u16(reader.u16()?);
        let headers = reader.headers()?;
        let content_type = reader.content_type()?;
        let body = reader.bytes_limited(MAX_BODY_BYTES)?;
        reader.finish()?;
        Ok(Self {
            response: FetchResponse {
                requested_url,
                final_url,
                redirect_chain,
                redirects,
                status,
                headers,
                content_type,
                body,
            },
            stored_at,
            freshness,
        })
    }

    /// Converts wall-clock metadata into a monotonic deadline.
    ///
    /// A clock that moved backwards treats the entry as age zero, while a
    /// clock that moved forwards consumes the corresponding freshness. This
    /// avoids deriving a negative duration from clock correction.
    #[must_use]
    pub fn fresh_until(&self, now: Instant, wall_now: SystemTime) -> Option<Instant> {
        if self.freshness.is_zero() {
            return None;
        }
        let age = wall_now
            .duration_since(self.stored_at)
            .unwrap_or(Duration::ZERO);
        let remaining = self.freshness.checked_sub(age)?;
        now.checked_add(remaining)
    }

    /// Whether this payload is fresh at the supplied wall/monotonic clocks.
    #[must_use]
    pub fn is_fresh(&self, now: Instant, wall_now: SystemTime) -> bool {
        self.fresh_until(now, wall_now)
            .is_some_and(|deadline| now < deadline)
    }
}

/// Errors raised while encoding or decoding a disk payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskCachePayloadError {
    InvalidFormat,
    InvalidUtf8,
    InvalidUrl,
    InvalidTime,
    Truncated,
    TrailingBytes,
    TooLarge,
}

impl fmt::Display for DiskCachePayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidFormat => "invalid disk cache payload format",
            Self::InvalidUtf8 => "disk cache payload contains invalid UTF-8",
            Self::InvalidUrl => "disk cache payload contains an invalid URL",
            Self::InvalidTime => "disk cache payload contains an invalid timestamp",
            Self::Truncated => "disk cache payload is truncated",
            Self::TrailingBytes => "disk cache payload has trailing bytes",
            Self::TooLarge => "disk cache payload exceeds its size limit",
        })
    }
}

impl std::error::Error for DiskCachePayloadError {}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Result<Vec<u8>, DiskCachePayloadError> {
        (self.bytes.len() <= MAX_PAYLOAD_BYTES)
            .then_some(self.bytes)
            .ok_or(DiskCachePayloadError::TooLarge)
    }

    fn reserve(&mut self, additional: usize) -> Result<(), DiskCachePayloadError> {
        let length = self
            .bytes
            .len()
            .checked_add(additional)
            .ok_or(DiskCachePayloadError::TooLarge)?;
        if length > MAX_PAYLOAD_BYTES {
            return Err(DiskCachePayloadError::TooLarge);
        }
        self.bytes.reserve(additional);
        Ok(())
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), DiskCachePayloadError> {
        self.reserve(value.len())?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn len(&mut self, length: usize) -> Result<(), DiskCachePayloadError> {
        self.u32(u32::try_from(length).map_err(|_| DiskCachePayloadError::TooLarge)?);
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), DiskCachePayloadError> {
        if value.len() > MAX_STRING_BYTES {
            return Err(DiskCachePayloadError::TooLarge);
        }
        self.len(value.len())?;
        self.bytes(value.as_bytes())
    }

    fn url(&mut self, value: &Url) -> Result<(), DiskCachePayloadError> {
        self.string(value.as_str())
    }

    fn urls(&mut self, values: &[Url]) -> Result<(), DiskCachePayloadError> {
        if values.len() > MAX_REDIRECTS {
            return Err(DiskCachePayloadError::TooLarge);
        }
        self.len(values.len())?;
        for value in values {
            self.url(value)?;
        }
        Ok(())
    }

    fn headers(&mut self, values: &[Header]) -> Result<(), DiskCachePayloadError> {
        if values.len() > MAX_HEADERS {
            return Err(DiskCachePayloadError::TooLarge);
        }
        self.len(values.len())?;
        for value in values {
            self.string(&value.name)?;
            self.bytes_limited(&value.value, MAX_STRING_BYTES)?;
        }
        Ok(())
    }

    fn redirects(&mut self, values: &[RedirectResponse]) -> Result<(), DiskCachePayloadError> {
        if values.len() > MAX_REDIRECTS {
            return Err(DiskCachePayloadError::TooLarge);
        }
        self.len(values.len())?;
        for value in values {
            self.url(&value.url)?;
            self.u16(value.status.as_u16());
            self.headers(&value.headers)?;
        }
        Ok(())
    }

    fn content_type(&mut self, value: Option<&ContentType>) -> Result<(), DiskCachePayloadError> {
        match value {
            Some(value) => {
                self.bytes(&[1])?;
                self.string(&value.media_type)?;
                match value.charset.as_deref() {
                    Some(charset) => {
                        self.bytes(&[1])?;
                        self.string(charset)?;
                    }
                    None => self.bytes(&[0])?,
                }
            }
            None => self.bytes(&[0])?,
        }
        Ok(())
    }

    fn bytes_limited(&mut self, value: &[u8], limit: usize) -> Result<(), DiskCachePayloadError> {
        if value.len() > limit {
            return Err(DiskCachePayloadError::TooLarge);
        }
        self.len(value.len())?;
        self.bytes(value)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DiskCachePayloadError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(DiskCachePayloadError::Truncated)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(DiskCachePayloadError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], DiskCachePayloadError> {
        self.take(length)
    }

    fn u16(&mut self) -> Result<u16, DiskCachePayloadError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("fixed-size integer"),
        ))
    }

    fn u32(&mut self) -> Result<u32, DiskCachePayloadError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("fixed-size integer"),
        ))
    }

    fn u64(&mut self) -> Result<u64, DiskCachePayloadError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("fixed-size integer"),
        ))
    }

    fn len(&mut self, limit: usize) -> Result<usize, DiskCachePayloadError> {
        let length = usize::try_from(self.u32()?).map_err(|_| DiskCachePayloadError::TooLarge)?;
        (length <= limit)
            .then_some(length)
            .ok_or(DiskCachePayloadError::TooLarge)
    }

    fn string(&mut self) -> Result<String, DiskCachePayloadError> {
        let length = self.len(MAX_STRING_BYTES)?;
        String::from_utf8(self.bytes(length)?.to_vec())
            .map_err(|_| DiskCachePayloadError::InvalidUtf8)
    }

    fn url(&mut self) -> Result<Url, DiskCachePayloadError> {
        self.string()?
            .parse()
            .map_err(|_| DiskCachePayloadError::InvalidUrl)
    }

    fn urls(&mut self) -> Result<Vec<Url>, DiskCachePayloadError> {
        let count = self.len(MAX_REDIRECTS)?;
        (0..count).map(|_| self.url()).collect()
    }

    fn headers(&mut self) -> Result<Vec<Header>, DiskCachePayloadError> {
        let count = self.len(MAX_HEADERS)?;
        (0..count)
            .map(|_| {
                Ok(Header {
                    name: self.string()?,
                    value: self.bytes_limited(MAX_STRING_BYTES)?,
                })
            })
            .collect()
    }

    fn redirects(&mut self) -> Result<Vec<RedirectResponse>, DiskCachePayloadError> {
        let count = self.len(MAX_REDIRECTS)?;
        (0..count)
            .map(|_| {
                Ok(RedirectResponse {
                    url: self.url()?,
                    status: HttpStatus::from_u16(self.u16()?),
                    headers: self.headers()?,
                })
            })
            .collect()
    }

    fn content_type(&mut self) -> Result<Option<ContentType>, DiskCachePayloadError> {
        match self.bytes(1)?[0] {
            0 => Ok(None),
            1 => {
                let media_type = self.string()?;
                let charset = match self.bytes(1)?[0] {
                    0 => None,
                    1 => Some(self.string()?),
                    _ => return Err(DiskCachePayloadError::InvalidFormat),
                };
                Ok(Some(ContentType {
                    media_type,
                    charset,
                }))
            }
            _ => Err(DiskCachePayloadError::InvalidFormat),
        }
    }

    fn bytes_limited(&mut self, limit: usize) -> Result<Vec<u8>, DiskCachePayloadError> {
        let length = self.len(limit)?;
        Ok(self.bytes(length)?.to_vec())
    }

    fn finish(&self) -> Result<(), DiskCachePayloadError> {
        (self.offset == self.bytes.len())
            .then_some(())
            .ok_or(DiskCachePayloadError::TrailingBytes)
    }
}

fn duration_parts(seconds: u64, nanos: u32) -> Result<Duration, DiskCachePayloadError> {
    (nanos < 1_000_000_000)
        .then_some(Duration::new(seconds, nanos))
        .ok_or(DiskCachePayloadError::InvalidTime)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use render_net::{ContentType, FetchResponse, Header, HttpStatus, Url};

    use super::{DiskCachePayload, DiskCachePayloadError};

    fn response() -> FetchResponse {
        let url = Url::parse("https://example.test/page").expect("test URL");
        FetchResponse {
            requested_url: url.clone(),
            final_url: url.clone(),
            redirect_chain: vec![url],
            redirects: Vec::new(),
            status: HttpStatus::from_u16(200),
            headers: vec![Header {
                name: "Cache-Control".into(),
                value: b"max-age=60".to_vec(),
            }],
            content_type: Some(ContentType {
                media_type: "text/html".into(),
                charset: Some("utf-8".into()),
            }),
            body: b"hello".to_vec(),
        }
    }

    #[test]
    fn round_trips_full_response_metadata() {
        let payload = DiskCachePayload::new(
            response(),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            Duration::from_secs(60),
        );
        let encoded = payload.encode().expect("encode payload");
        let decoded = DiskCachePayload::decode(&encoded).expect("decode payload");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn rejects_corruption_and_trailing_data() {
        let payload = DiskCachePayload::new(response(), SystemTime::now(), Duration::from_secs(1));
        let mut encoded = payload.encode().expect("encode payload");
        encoded.push(1);
        assert_eq!(
            DiskCachePayload::decode(&encoded),
            Err(DiskCachePayloadError::TrailingBytes)
        );
        encoded[0] ^= 1;
        assert_eq!(
            DiskCachePayload::decode(&encoded),
            Err(DiskCachePayloadError::InvalidFormat)
        );
    }

    #[test]
    fn reconstructs_freshness_without_serializing_instant() {
        let stored_at = SystemTime::now() - Duration::from_secs(2);
        let payload = DiskCachePayload::new(response(), stored_at, Duration::from_secs(5));
        let now = Instant::now();
        assert!(payload.is_fresh(now, SystemTime::now()));
        assert!(!payload.is_fresh(now, SystemTime::now() + Duration::from_secs(10)));
    }

    #[test]
    fn rejects_invalid_urls_and_oversized_body() {
        let mut bytes = DiskCachePayload::new(response(), SystemTime::now(), Duration::ZERO)
            .encode()
            .expect("encode payload");
        // The requested URL starts after the fixed timestamp/freshness fields
        // and its length prefix. Replace its bytes with an invalid URL while
        // keeping the record structurally valid.
        let requested_url_length_offset = 8 + 8 + 4 + 8 + 4;
        let requested_url_bytes_offset = requested_url_length_offset + 4;
        let invalid_url = b"not a URL";
        let original_length = u32::from_le_bytes(
            bytes[requested_url_length_offset..requested_url_length_offset + 4]
                .try_into()
                .expect("URL length bytes"),
        ) as usize;
        assert_eq!(original_length, response().requested_url.as_str().len());
        bytes[requested_url_bytes_offset..requested_url_bytes_offset + invalid_url.len()]
            .copy_from_slice(invalid_url);
        bytes[requested_url_length_offset..requested_url_length_offset + 4]
            .copy_from_slice(&(invalid_url.len() as u32).to_le_bytes());
        assert_eq!(
            DiskCachePayload::decode(&bytes),
            Err(DiskCachePayloadError::InvalidUrl)
        );

        let oversized = FetchResponse {
            body: vec![0; 32 * 1024 * 1024 + 1],
            ..response()
        };
        assert_eq!(
            DiskCachePayload::new(oversized, SystemTime::now(), Duration::ZERO).encode(),
            Err(DiskCachePayloadError::TooLarge)
        );
    }
}
