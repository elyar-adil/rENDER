//! Bounded HTML media resource discovery.
//!
//! This module performs no I/O and does not decode media. Browser coordinators
//! may use the discovered candidates to schedule requests, then use the source
//! snapshots to reject responses for media elements whose source changed.

use url::Url;

use crate::dom::{Dom, DomRevision, ElementData, Namespace, NodeId, NodeKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaLimits {
    pub max_discovery_nodes: usize,
    pub max_media_elements: usize,
    pub max_source_candidates: usize,
    pub max_candidates_per_media: usize,
    pub max_url_bytes: usize,
}

impl Default for MediaLimits {
    fn default() -> Self {
        Self {
            max_discovery_nodes: 1_000_000,
            max_media_elements: 1_024,
            max_source_candidates: 4_096,
            max_candidates_per_media: 64,
            max_url_bytes: 64 * 1_024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Audio,
    Video,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaPreload {
    None,
    Metadata,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaBooleanAttribute {
    Absent,
    Present,
}

impl MediaBooleanAttribute {
    #[must_use]
    pub const fn is_present(self) -> bool {
        matches!(self, Self::Present)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaDiscoveryDiagnosticCode {
    NodeLimit,
    MediaElementLimit,
    SourceCandidateLimit,
    CandidatesPerMediaLimit,
    UrlBytesLimit,
    InvalidBaseUrl,
    MissingSource,
    EmptySource,
    InvalidSourceUrl,
    UnsupportedScheme,
    InvalidPreload,
    EmptyPoster,
    InvalidPosterUrl,
    UnsupportedPosterScheme,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaDiscoveryDiagnostic {
    pub node: Option<NodeId>,
    pub code: MediaDiscoveryDiagnosticCode,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MediaSourceKey {
    pub owner: NodeId,
    pub source_node: NodeId,
    pub kind: MediaKind,
    pub requested_url: Url,
    pub source_snapshot: String,
    pub type_snapshot: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSourceCandidate {
    pub source_order: usize,
    pub candidate_order: usize,
    pub key: MediaSourceKey,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MediaPosterKey {
    pub owner: NodeId,
    pub requested_url: Url,
    pub source_snapshot: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredMedia {
    pub source_order: usize,
    pub owner: NodeId,
    pub kind: MediaKind,
    pub preload: MediaPreload,
    pub controls: MediaBooleanAttribute,
    pub autoplay: MediaBooleanAttribute,
    pub muted: MediaBooleanAttribute,
    pub looping: MediaBooleanAttribute,
    pub poster: Option<MediaPosterKey>,
    pub candidates: Vec<MediaSourceCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaDiscovery {
    pub revision: DomRevision,
    pub effective_base_url: Url,
    pub media: Vec<DiscoveredMedia>,
    pub diagnostics: Vec<MediaDiscoveryDiagnostic>,
}

/// Discover fetchable HTML `audio` and `video` sources in tree order.
///
/// A media element's own `src` is the first candidate, followed by its direct
/// HTML `source` children. Only HTTP(S) URLs are retained.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the tree-order loop keeps media and direct source-child discovery atomic"
)]
pub fn discover_media(dom: &Dom, document_url: &Url, limits: MediaLimits) -> MediaDiscovery {
    let (elements, mut diagnostics) = collect_elements(dom, limits.max_discovery_nodes);
    let effective_base_url = effective_base_url(dom, &elements, document_url, &mut diagnostics);
    let mut media = Vec::new();
    let mut total_candidates = 0_usize;

    for node in &elements {
        let Some(element) = html_element(dom, *node) else {
            continue;
        };
        let kind = match element.local_name.as_str() {
            "audio" => MediaKind::Audio,
            "video" => MediaKind::Video,
            _ => continue,
        };
        if media.len() >= limits.max_media_elements {
            diagnostics.push(MediaDiscoveryDiagnostic {
                node: Some(*node),
                code: MediaDiscoveryDiagnosticCode::MediaElementLimit,
                message: format!(
                    "media element limit ({}) exceeded",
                    limits.max_media_elements
                ),
            });
            continue;
        }

        let preload = parse_preload(element, *node, &mut diagnostics);
        let poster = if kind == MediaKind::Video {
            discover_poster(
                *node,
                element,
                &effective_base_url,
                limits,
                &mut diagnostics,
            )
        } else {
            None
        };
        let mut candidates = Vec::new();
        if let Some(source) = attribute(element, "src") {
            add_candidate(
                *node,
                *node,
                kind,
                source,
                attribute(element, "type"),
                &effective_base_url,
                limits,
                &mut total_candidates,
                &mut candidates,
                &mut diagnostics,
            );
        }
        for child in dom.children(*node).unwrap_or_default() {
            let Some(source_element) = html_element(dom, *child) else {
                continue;
            };
            if source_element.local_name != "source" {
                continue;
            }
            let Some(source) = attribute(source_element, "src") else {
                diagnostics.push(MediaDiscoveryDiagnostic {
                    node: Some(*child),
                    code: MediaDiscoveryDiagnosticCode::MissingSource,
                    message: "media source has no src attribute".to_owned(),
                });
                continue;
            };
            add_candidate(
                *node,
                *child,
                kind,
                source,
                attribute(source_element, "type"),
                &effective_base_url,
                limits,
                &mut total_candidates,
                &mut candidates,
                &mut diagnostics,
            );
        }
        if candidates.is_empty() && attribute(element, "src").is_none() {
            let has_source_child = dom.children(*node).unwrap_or_default().iter().any(|child| {
                html_element(dom, *child).is_some_and(|child| child.local_name == "source")
            });
            if !has_source_child {
                diagnostics.push(MediaDiscoveryDiagnostic {
                    node: Some(*node),
                    code: MediaDiscoveryDiagnosticCode::MissingSource,
                    message: "media element has no src or source children".to_owned(),
                });
            }
        }

        media.push(DiscoveredMedia {
            source_order: media.len(),
            owner: *node,
            kind,
            preload,
            controls: boolean_attribute(element, "controls"),
            autoplay: boolean_attribute(element, "autoplay"),
            muted: boolean_attribute(element, "muted"),
            looping: boolean_attribute(element, "loop"),
            poster,
            candidates,
        });
    }

    MediaDiscovery {
        revision: dom.revision(),
        effective_base_url,
        media,
        diagnostics,
    }
}

/// Whether a completed request still represents the same media candidate.
#[must_use]
pub fn media_source_key_is_current(dom: &Dom, document_url: &Url, key: &MediaSourceKey) -> bool {
    let Some(owner) = html_element(dom, key.owner) else {
        return false;
    };
    if media_kind(owner) != Some(key.kind) {
        return false;
    }
    let Some(source) = html_element(dom, key.source_node) else {
        return false;
    };
    if key.source_node == key.owner {
        if source.local_name != owner.local_name {
            return false;
        }
    } else if source.local_name != "source" || dom.parent(key.source_node) != Some(key.owner) {
        return false;
    }
    if attribute(source, "src") != Some(key.source_snapshot.as_str())
        || attribute(source, "type") != key.type_snapshot.as_deref()
    {
        return false;
    }
    current_url(dom, document_url, &key.source_snapshot, &key.requested_url)
}

/// Whether a completed poster request still represents the video's `poster`.
#[must_use]
pub fn media_poster_key_is_current(dom: &Dom, document_url: &Url, key: &MediaPosterKey) -> bool {
    let Some(owner) = html_element(dom, key.owner) else {
        return false;
    };
    if owner.local_name != "video"
        || attribute(owner, "poster") != Some(key.source_snapshot.as_str())
    {
        return false;
    }
    current_url(dom, document_url, &key.source_snapshot, &key.requested_url)
}

fn parse_preload(
    element: &ElementData,
    node: NodeId,
    diagnostics: &mut Vec<MediaDiscoveryDiagnostic>,
) -> MediaPreload {
    let Some(value) = attribute(element, "preload") else {
        return MediaPreload::Auto;
    };
    match value.to_ascii_lowercase().as_str() {
        "" | "auto" => MediaPreload::Auto,
        "none" => MediaPreload::None,
        "metadata" => MediaPreload::Metadata,
        _ => {
            diagnostics.push(MediaDiscoveryDiagnostic {
                node: Some(node),
                code: MediaDiscoveryDiagnosticCode::InvalidPreload,
                message: format!("invalid media preload value '{value}'; using metadata"),
            });
            MediaPreload::Metadata
        }
    }
}

fn discover_poster(
    owner: NodeId,
    element: &ElementData,
    base_url: &Url,
    limits: MediaLimits,
    diagnostics: &mut Vec<MediaDiscoveryDiagnostic>,
) -> Option<MediaPosterKey> {
    let source = attribute(element, "poster")?;
    if source.is_empty() {
        diagnostics.push(MediaDiscoveryDiagnostic {
            node: Some(owner),
            code: MediaDiscoveryDiagnosticCode::EmptyPoster,
            message: "empty video poster is not fetched".to_owned(),
        });
        return None;
    }
    let requested_url = resolve_url(
        owner,
        source,
        "video poster",
        base_url,
        limits,
        MediaDiscoveryDiagnosticCode::InvalidPosterUrl,
        MediaDiscoveryDiagnosticCode::UnsupportedPosterScheme,
        diagnostics,
    )?;
    Some(MediaPosterKey {
        owner,
        requested_url,
        source_snapshot: source.to_owned(),
    })
}

#[allow(clippy::too_many_arguments)]
fn add_candidate(
    owner: NodeId,
    source_node: NodeId,
    kind: MediaKind,
    source: &str,
    type_hint: Option<&str>,
    base_url: &Url,
    limits: MediaLimits,
    total_candidates: &mut usize,
    candidates: &mut Vec<MediaSourceCandidate>,
    diagnostics: &mut Vec<MediaDiscoveryDiagnostic>,
) {
    if source.is_empty() {
        diagnostics.push(MediaDiscoveryDiagnostic {
            node: Some(source_node),
            code: MediaDiscoveryDiagnosticCode::EmptySource,
            message: "empty media src is not fetched".to_owned(),
        });
        return;
    }
    if candidates.len() >= limits.max_candidates_per_media {
        diagnostics.push(MediaDiscoveryDiagnostic {
            node: Some(source_node),
            code: MediaDiscoveryDiagnosticCode::CandidatesPerMediaLimit,
            message: format!(
                "media candidate limit ({}) exceeded for one element",
                limits.max_candidates_per_media
            ),
        });
        return;
    }
    if *total_candidates >= limits.max_source_candidates {
        diagnostics.push(MediaDiscoveryDiagnostic {
            node: Some(source_node),
            code: MediaDiscoveryDiagnosticCode::SourceCandidateLimit,
            message: format!(
                "media source candidate limit ({}) exceeded",
                limits.max_source_candidates
            ),
        });
        return;
    }
    let Some(requested_url) = resolve_url(
        source_node,
        source,
        "media source",
        base_url,
        limits,
        MediaDiscoveryDiagnosticCode::InvalidSourceUrl,
        MediaDiscoveryDiagnosticCode::UnsupportedScheme,
        diagnostics,
    ) else {
        return;
    };
    let candidate_order = candidates.len();
    candidates.push(MediaSourceCandidate {
        source_order: *total_candidates,
        candidate_order,
        key: MediaSourceKey {
            owner,
            source_node,
            kind,
            requested_url,
            source_snapshot: source.to_owned(),
            type_snapshot: type_hint.map(str::to_owned),
        },
    });
    *total_candidates += 1;
}

#[allow(clippy::too_many_arguments)]
fn resolve_url(
    node: NodeId,
    source: &str,
    label: &str,
    base_url: &Url,
    limits: MediaLimits,
    invalid_code: MediaDiscoveryDiagnosticCode,
    scheme_code: MediaDiscoveryDiagnosticCode,
    diagnostics: &mut Vec<MediaDiscoveryDiagnostic>,
) -> Option<Url> {
    if source.len() > limits.max_url_bytes {
        diagnostics.push(MediaDiscoveryDiagnostic {
            node: Some(node),
            code: MediaDiscoveryDiagnosticCode::UrlBytesLimit,
            message: format!(
                "{label} uses {} URL bytes; limit is {}",
                source.len(),
                limits.max_url_bytes
            ),
        });
        return None;
    }
    let Ok(requested_url) = base_url.join(source) else {
        diagnostics.push(MediaDiscoveryDiagnostic {
            node: Some(node),
            code: invalid_code,
            message: format!("{label} '{source}' is not a valid URL reference"),
        });
        return None;
    };
    if requested_url.as_str().len() > limits.max_url_bytes {
        diagnostics.push(MediaDiscoveryDiagnostic {
            node: Some(node),
            code: MediaDiscoveryDiagnosticCode::UrlBytesLimit,
            message: format!(
                "resolved {label} URL uses {} bytes; limit is {}",
                requested_url.as_str().len(),
                limits.max_url_bytes
            ),
        });
        return None;
    }
    if !matches!(requested_url.scheme(), "http" | "https") {
        diagnostics.push(MediaDiscoveryDiagnostic {
            node: Some(node),
            code: scheme_code,
            message: format!(
                "{label} URL scheme '{}' is not supported",
                requested_url.scheme()
            ),
        });
        return None;
    }
    Some(requested_url)
}

fn current_url(dom: &Dom, document_url: &Url, source: &str, expected: &Url) -> bool {
    let (elements, _) = collect_elements(dom, usize::MAX);
    let mut diagnostics = Vec::new();
    let base = effective_base_url(dom, &elements, document_url, &mut diagnostics);
    base.join(source).is_ok_and(|url| url == *expected)
}

fn collect_elements(dom: &Dom, limit: usize) -> (Vec<NodeId>, Vec<MediaDiscoveryDiagnostic>) {
    let mut elements = Vec::new();
    let mut diagnostics = Vec::new();
    let mut pending = vec![dom.document()];
    let mut visited = 0_usize;
    while let Some(node) = pending.pop() {
        if visited >= limit {
            diagnostics.push(MediaDiscoveryDiagnostic {
                node: Some(node),
                code: MediaDiscoveryDiagnosticCode::NodeLimit,
                message: format!("media discovery node limit ({limit}) exceeded"),
            });
            break;
        }
        visited += 1;
        let Some(current) = dom.node(node) else {
            continue;
        };
        if matches!(current.kind(), NodeKind::Element(_)) {
            elements.push(node);
        }
        pending.extend(current.children().iter().rev());
    }
    (elements, diagnostics)
}

fn effective_base_url(
    dom: &Dom,
    elements: &[NodeId],
    document_url: &Url,
    diagnostics: &mut Vec<MediaDiscoveryDiagnostic>,
) -> Url {
    for node in elements {
        let Some(element) = html_element(dom, *node) else {
            continue;
        };
        if element.local_name != "base" {
            continue;
        }
        let Some(href) = attribute(element, "href") else {
            continue;
        };
        match document_url.join(href) {
            Ok(url) => return url,
            Err(error) => diagnostics.push(MediaDiscoveryDiagnostic {
                node: Some(*node),
                code: MediaDiscoveryDiagnosticCode::InvalidBaseUrl,
                message: format!("base href '{href}' is invalid: {error}"),
            }),
        }
    }
    document_url.clone()
}

fn media_kind(element: &ElementData) -> Option<MediaKind> {
    match element.local_name.as_str() {
        "audio" => Some(MediaKind::Audio),
        "video" => Some(MediaKind::Video),
        _ => None,
    }
}

fn html_element(dom: &Dom, node: NodeId) -> Option<&ElementData> {
    let NodeKind::Element(element) = dom.node(node)?.kind() else {
        return None;
    };
    (element.namespace == Namespace::Html).then_some(element)
}

fn boolean_attribute(element: &ElementData, name: &str) -> MediaBooleanAttribute {
    if attribute(element, name).is_some() {
        MediaBooleanAttribute::Present
    } else {
        MediaBooleanAttribute::Absent
    }
}

fn attribute<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.namespace.is_none() && attribute.local_name == name)
        .map(|attribute| attribute.value.as_str())
}

#[cfg(test)]
mod tests {
    use url::Url;

    use crate::html::parse_document;

    use super::{
        MediaDiscoveryDiagnosticCode, MediaKind, MediaLimits, MediaPreload, discover_media,
        media_poster_key_is_current, media_source_key_is_current,
    };

    #[test]
    fn discovers_bilibili_style_video_metadata() {
        let parsed = parse_document(
            "<video controls preload=metadata poster='/assets/poster.png'>\
             <source src='/media/bilibili-init.mp4' type='video/mp4'></video>",
        );
        let document_url = Url::parse("https://www.bilibili.com/video/BV1").unwrap();
        let discovery = discover_media(&parsed.dom, &document_url, MediaLimits::default());

        assert_eq!(discovery.media.len(), 1);
        let video = &discovery.media[0];
        assert_eq!(video.kind, MediaKind::Video);
        assert_eq!(video.preload, MediaPreload::Metadata);
        assert!(video.controls.is_present());
        assert!(!video.autoplay.is_present());
        assert_eq!(
            video.poster.as_ref().unwrap().requested_url.as_str(),
            "https://www.bilibili.com/assets/poster.png"
        );
        assert_eq!(video.candidates.len(), 1);
        assert_eq!(
            video.candidates[0].key.type_snapshot.as_deref(),
            Some("video/mp4")
        );
        assert_eq!(
            video.candidates[0].key.requested_url.as_str(),
            "https://www.bilibili.com/media/bilibili-init.mp4"
        );
    }

    #[test]
    fn applies_base_and_preserves_parent_then_child_source_order() {
        let parsed = parse_document(
            "<base href='https://cdn.example/media/'><video src='primary.mp4' autoplay muted loop>\
             <source src='fallback.webm' type='video/webm'>\
             <source src='fallback.mp4' type='video/mp4'></video>",
        );
        let document_url = Url::parse("https://example.test/watch").unwrap();
        let discovery = discover_media(&parsed.dom, &document_url, MediaLimits::default());
        let video = &discovery.media[0];

        assert!(video.autoplay.is_present());
        assert!(video.muted.is_present());
        assert!(video.looping.is_present());
        assert_eq!(
            video
                .candidates
                .iter()
                .map(|candidate| candidate.key.requested_url.as_str())
                .collect::<Vec<_>>(),
            vec![
                "https://cdn.example/media/primary.mp4",
                "https://cdn.example/media/fallback.webm",
                "https://cdn.example/media/fallback.mp4",
            ]
        );
        assert_eq!(
            video
                .candidates
                .iter()
                .map(|candidate| candidate.candidate_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn enforces_node_media_candidate_and_url_limits() {
        let parsed = parse_document(
            "<video src='first.mp4'><source src='second.mp4'></video>\
             <audio src='third.mp3'></audio>",
        );
        let document_url = Url::parse("https://example.test/").unwrap();
        let discovery = discover_media(
            &parsed.dom,
            &document_url,
            MediaLimits {
                max_media_elements: 1,
                max_source_candidates: 10,
                max_candidates_per_media: 1,
                max_url_bytes: 100,
                ..MediaLimits::default()
            },
        );
        let codes = discovery
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert!(codes.contains(&MediaDiscoveryDiagnosticCode::CandidatesPerMediaLimit));
        assert!(codes.contains(&MediaDiscoveryDiagnosticCode::MediaElementLimit));

        let candidate_limited = discover_media(
            &parsed.dom,
            &document_url,
            MediaLimits {
                max_source_candidates: 1,
                max_candidates_per_media: 10,
                ..MediaLimits::default()
            },
        );
        assert!(candidate_limited.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == MediaDiscoveryDiagnosticCode::SourceCandidateLimit
        }));

        let url_limited = discover_media(
            &parsed.dom,
            &document_url,
            MediaLimits {
                max_url_bytes: 10,
                ..MediaLimits::default()
            },
        );
        assert!(
            url_limited.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == MediaDiscoveryDiagnosticCode::UrlBytesLimit
            })
        );

        let node_limited = discover_media(
            &parsed.dom,
            &document_url,
            MediaLimits {
                max_discovery_nodes: 1,
                ..MediaLimits::default()
            },
        );
        assert!(
            node_limited
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == MediaDiscoveryDiagnosticCode::NodeLimit })
        );
    }

    #[test]
    fn rejects_stale_source_type_poster_and_base_without_unrelated_revision_rejection() {
        let mut parsed = parse_document(
            "<base href='https://cdn.example/'><video id=player poster='poster.png'>\
             <source id=source src='movie.mp4' type='video/mp4'></video>",
        );
        let document_url = Url::parse("https://example.test/watch").unwrap();
        let discovery = discover_media(&parsed.dom, &document_url, MediaLimits::default());
        let video = &discovery.media[0];
        let source_key = video.candidates[0].key.clone();
        let poster_key = video.poster.clone().unwrap();

        parsed
            .dom
            .set_attribute(video.owner, "controls", "")
            .unwrap();
        assert!(media_source_key_is_current(
            &parsed.dom,
            &document_url,
            &source_key
        ));
        assert!(media_poster_key_is_current(
            &parsed.dom,
            &document_url,
            &poster_key
        ));
        parsed
            .dom
            .set_attribute(source_key.source_node, "type", "video/webm")
            .unwrap();
        assert!(!media_source_key_is_current(
            &parsed.dom,
            &document_url,
            &source_key
        ));
        parsed
            .dom
            .set_attribute(video.owner, "poster", "new.png")
            .unwrap();
        assert!(!media_poster_key_is_current(
            &parsed.dom,
            &document_url,
            &poster_key
        ));
    }
}
