//! Reflected IDL attributes of the non-form HTML element interfaces.
//!
//! One row per IDL attribute: the IDL name, the content attribute it reflects,
//! and the rule of `Reflection`. Rows are taken from the element tables of the
//! WPT reflection suite (`html/dom/elements-*.js`, pinned at
//! c7fdee80f3f17b4e9813964916afdfd57ace863f). `HTMLMediaElement` holds the
//! attributes that video and audio share, and `Element` the ARIA mixin rows.
//! Form-control, focus and token-list rows are not here.

use super::reflect::{Keywords, Reflected, Reflection};

const ARIA_ATOMIC: Keywords = Keywords {
    values: &["true", "false"],
    invalid: Some("false"),
    missing: None,
    nullable: true,
};

const ARIA_AUTO_COMPLETE: Keywords = Keywords {
    values: &["inline", "list", "both", "none"],
    invalid: Some("none"),
    missing: Some("none"),
    nullable: true,
};

const ARIA_BUSY: Keywords = Keywords {
    values: &["true", "false"],
    invalid: Some("false"),
    missing: Some("false"),
    nullable: true,
};

const ARIA_CHECKED: Keywords = Keywords {
    values: &["true", "false", "mixed"],
    invalid: None,
    missing: None,
    nullable: true,
};

const ARIA_CURRENT: Keywords = Keywords {
    values: &["page", "step", "location", "date", "time", "true", "false"],
    invalid: Some("true"),
    missing: Some("false"),
    nullable: true,
};

const ARIA_EXPANDED: Keywords = Keywords {
    values: &["true", "false"],
    invalid: None,
    missing: None,
    nullable: true,
};

const ARIA_HAS_POPUP: Keywords = Keywords {
    values: &["true", "false", "menu", "dialog", "listbox", "tree", "grid"],
    invalid: Some("false"),
    missing: None,
    nullable: true,
};

const ARIA_INVALID: Keywords = Keywords {
    values: &["true", "false", "spelling", "grammar"],
    invalid: Some("true"),
    missing: Some("false"),
    nullable: true,
};

const ARIA_LIVE: Keywords = Keywords {
    values: &["polite", "assertive", "off"],
    invalid: Some("off"),
    missing: Some("off"),
    nullable: true,
};

const ARIA_ORIENTATION: Keywords = Keywords {
    values: &["horizontal", "vertical"],
    invalid: None,
    missing: None,
    nullable: true,
};

const ARIA_SORT: Keywords = Keywords {
    values: &["ascending", "descending", "other", "none"],
    invalid: Some("none"),
    missing: Some("none"),
    nullable: true,
};

const AS: Keywords = Keywords {
    values: &[
        "fetch",
        "audio",
        "document",
        "embed",
        "font",
        "image",
        "manifest",
        "object",
        "report",
        "script",
        "sharedworker",
        "style",
        "track",
        "video",
        "worker",
        "xslt",
    ],
    invalid: None,
    missing: None,
    nullable: false,
};

const BEHAVIOR: Keywords = Keywords {
    values: &["scroll", "slide", "alternate"],
    invalid: Some("scroll"),
    missing: Some("scroll"),
    nullable: false,
};

const CROSS_ORIGIN: Keywords = Keywords {
    values: &["anonymous", "use-credentials"],
    invalid: Some("anonymous"),
    missing: None,
    nullable: true,
};

const DECODING: Keywords = Keywords {
    values: &["async", "sync", "auto"],
    invalid: Some("auto"),
    missing: Some("auto"),
    nullable: false,
};

const DIRECTION: Keywords = Keywords {
    values: &["up", "right", "down", "left"],
    invalid: Some("left"),
    missing: Some("left"),
    nullable: false,
};

const KIND: Keywords = Keywords {
    values: &[
        "subtitles",
        "captions",
        "descriptions",
        "chapters",
        "metadata",
    ],
    invalid: Some("metadata"),
    missing: Some("subtitles"),
    nullable: false,
};

const LOADING: Keywords = Keywords {
    values: &["lazy", "eager"],
    invalid: Some("eager"),
    missing: Some("eager"),
    nullable: false,
};

const PRELOAD: Keywords = Keywords {
    values: &["none", "metadata", "auto"],
    invalid: Some("auto"),
    missing: Some("metadata"),
    nullable: false,
};

const REFERRER_POLICY: Keywords = Keywords {
    values: &[
        "",
        "no-referrer",
        "no-referrer-when-downgrade",
        "same-origin",
        "origin",
        "strict-origin",
        "origin-when-cross-origin",
        "strict-origin-when-cross-origin",
        "unsafe-url",
    ],
    invalid: None,
    missing: None,
    nullable: false,
};

const SCOPE: Keywords = Keywords {
    values: &["row", "col", "rowgroup", "colgroup"],
    invalid: None,
    missing: None,
    nullable: false,
};

pub(super) const INTERFACES: &[(&str, &[Reflected])] = &[
    (
        "Element",
        &[
            // DOM §4.2.2.2: the `slot` IDL attribute reflects the `slot` content
            // attribute, which names the slot a child is assigned to.
            Reflected {
                idl: "slot",
                attribute: "slot",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "ariaAtomic",
                attribute: "aria-atomic",
                reflection: Reflection::Keyword(ARIA_ATOMIC),
            },
            Reflected {
                idl: "ariaAutoComplete",
                attribute: "aria-autocomplete",
                reflection: Reflection::Keyword(ARIA_AUTO_COMPLETE),
            },
            Reflected {
                idl: "ariaBusy",
                attribute: "aria-busy",
                reflection: Reflection::Keyword(ARIA_BUSY),
            },
            Reflected {
                idl: "ariaChecked",
                attribute: "aria-checked",
                reflection: Reflection::Keyword(ARIA_CHECKED),
            },
            Reflected {
                idl: "ariaCurrent",
                attribute: "aria-current",
                reflection: Reflection::Keyword(ARIA_CURRENT),
            },
            Reflected {
                idl: "ariaDisabled",
                attribute: "aria-disabled",
                reflection: Reflection::Keyword(ARIA_BUSY),
            },
            Reflected {
                idl: "ariaExpanded",
                attribute: "aria-expanded",
                reflection: Reflection::Keyword(ARIA_EXPANDED),
            },
            Reflected {
                idl: "ariaHasPopup",
                attribute: "aria-haspopup",
                reflection: Reflection::Keyword(ARIA_HAS_POPUP),
            },
            Reflected {
                idl: "ariaHidden",
                attribute: "aria-hidden",
                reflection: Reflection::Keyword(ARIA_BUSY),
            },
            Reflected {
                idl: "ariaInvalid",
                attribute: "aria-invalid",
                reflection: Reflection::Keyword(ARIA_INVALID),
            },
            Reflected {
                idl: "ariaLive",
                attribute: "aria-live",
                reflection: Reflection::Keyword(ARIA_LIVE),
            },
            Reflected {
                idl: "ariaModal",
                attribute: "aria-modal",
                reflection: Reflection::Keyword(ARIA_BUSY),
            },
            Reflected {
                idl: "ariaMultiLine",
                attribute: "aria-multiline",
                reflection: Reflection::Keyword(ARIA_BUSY),
            },
            Reflected {
                idl: "ariaMultiSelectable",
                attribute: "aria-multiselectable",
                reflection: Reflection::Keyword(ARIA_BUSY),
            },
            Reflected {
                idl: "ariaOrientation",
                attribute: "aria-orientation",
                reflection: Reflection::Keyword(ARIA_ORIENTATION),
            },
            Reflected {
                idl: "ariaPressed",
                attribute: "aria-pressed",
                reflection: Reflection::Keyword(ARIA_CHECKED),
            },
            Reflected {
                idl: "ariaReadOnly",
                attribute: "aria-readonly",
                reflection: Reflection::Keyword(ARIA_BUSY),
            },
            Reflected {
                idl: "ariaRequired",
                attribute: "aria-required",
                reflection: Reflection::Keyword(ARIA_BUSY),
            },
            Reflected {
                idl: "ariaSelected",
                attribute: "aria-selected",
                reflection: Reflection::Keyword(ARIA_EXPANDED),
            },
            Reflected {
                idl: "ariaSort",
                attribute: "aria-sort",
                reflection: Reflection::Keyword(ARIA_SORT),
            },
        ],
    ),
    (
        "HTMLImageElement",
        &[
            Reflected {
                idl: "alt",
                attribute: "alt",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "src",
                attribute: "src",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "srcset",
                attribute: "srcset",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "crossOrigin",
                attribute: "crossorigin",
                reflection: Reflection::Keyword(CROSS_ORIGIN),
            },
            Reflected {
                idl: "useMap",
                attribute: "usemap",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "isMap",
                attribute: "ismap",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "height",
                attribute: "height",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "referrerPolicy",
                attribute: "referrerpolicy",
                reflection: Reflection::Keyword(REFERRER_POLICY),
            },
            Reflected {
                idl: "decoding",
                attribute: "decoding",
                reflection: Reflection::Keyword(DECODING),
            },
            Reflected {
                idl: "name",
                attribute: "name",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "lowsrc",
                attribute: "lowsrc",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "hspace",
                attribute: "hspace",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "vspace",
                attribute: "vspace",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "longDesc",
                attribute: "longdesc",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "border",
                attribute: "border",
                reflection: Reflection::NullAsEmpty,
            },
        ],
    ),
    (
        "HTMLIFrameElement",
        &[
            Reflected {
                idl: "src",
                attribute: "src",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "srcdoc",
                attribute: "srcdoc",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "name",
                attribute: "name",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "allowFullscreen",
                attribute: "allowfullscreen",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "height",
                attribute: "height",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "referrerPolicy",
                attribute: "referrerpolicy",
                reflection: Reflection::Keyword(REFERRER_POLICY),
            },
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "scrolling",
                attribute: "scrolling",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "frameBorder",
                attribute: "frameborder",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "longDesc",
                attribute: "longdesc",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "marginHeight",
                attribute: "marginheight",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "marginWidth",
                attribute: "marginwidth",
                reflection: Reflection::NullAsEmpty,
            },
        ],
    ),
    (
        "HTMLEmbedElement",
        &[
            Reflected {
                idl: "src",
                attribute: "src",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "height",
                attribute: "height",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "name",
                attribute: "name",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLObjectElement",
        &[
            Reflected {
                idl: "data",
                attribute: "data",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "name",
                attribute: "name",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "useMap",
                attribute: "usemap",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "height",
                attribute: "height",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "archive",
                attribute: "archive",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "code",
                attribute: "code",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "declare",
                attribute: "declare",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "hspace",
                attribute: "hspace",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "standby",
                attribute: "standby",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "vspace",
                attribute: "vspace",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "codeBase",
                attribute: "codebase",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "codeType",
                attribute: "codetype",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "border",
                attribute: "border",
                reflection: Reflection::NullAsEmpty,
            },
        ],
    ),
    (
        "HTMLParamElement",
        &[
            Reflected {
                idl: "name",
                attribute: "name",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "value",
                attribute: "value",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "valueType",
                attribute: "valuetype",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLMediaElement",
        &[
            Reflected {
                idl: "src",
                attribute: "src",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "crossOrigin",
                attribute: "crossorigin",
                reflection: Reflection::Keyword(CROSS_ORIGIN),
            },
            Reflected {
                idl: "preload",
                attribute: "preload",
                reflection: Reflection::Keyword(PRELOAD),
            },
            Reflected {
                idl: "autoplay",
                attribute: "autoplay",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "loop",
                attribute: "loop",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "controls",
                attribute: "controls",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "defaultMuted",
                attribute: "muted",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "loading",
                attribute: "loading",
                reflection: Reflection::Keyword(LOADING),
            },
        ],
    ),
    (
        "HTMLVideoElement",
        &[
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "height",
                attribute: "height",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "poster",
                attribute: "poster",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "playsInline",
                attribute: "playsinline",
                reflection: Reflection::Boolean,
            },
        ],
    ),
    (
        "HTMLSourceElement",
        &[
            Reflected {
                idl: "src",
                attribute: "src",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "srcset",
                attribute: "srcset",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "sizes",
                attribute: "sizes",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "media",
                attribute: "media",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLTrackElement",
        &[
            Reflected {
                idl: "kind",
                attribute: "kind",
                reflection: Reflection::Keyword(KIND),
            },
            Reflected {
                idl: "src",
                attribute: "src",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "srclang",
                attribute: "srclang",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "label",
                attribute: "label",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "default",
                attribute: "default",
                reflection: Reflection::Boolean,
            },
        ],
    ),
    (
        "HTMLCanvasElement",
        &[
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::UnsignedLong {
                    default: 300,
                    clamp: None,
                },
            },
            Reflected {
                idl: "height",
                attribute: "height",
                reflection: Reflection::UnsignedLong {
                    default: 150,
                    clamp: None,
                },
            },
        ],
    ),
    (
        "HTMLMapElement",
        &[Reflected {
            idl: "name",
            attribute: "name",
            reflection: Reflection::String,
        }],
    ),
    (
        "HTMLAreaElement",
        &[
            Reflected {
                idl: "alt",
                attribute: "alt",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "coords",
                attribute: "coords",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "shape",
                attribute: "shape",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "target",
                attribute: "target",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "download",
                attribute: "download",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "ping",
                attribute: "ping",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "rel",
                attribute: "rel",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "referrerPolicy",
                attribute: "referrerpolicy",
                reflection: Reflection::Keyword(REFERRER_POLICY),
            },
            Reflected {
                idl: "hreflang",
                attribute: "hreflang",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "href",
                attribute: "href",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "noHref",
                attribute: "nohref",
                reflection: Reflection::Boolean,
            },
        ],
    ),
    (
        "HTMLParagraphElement",
        &[Reflected {
            idl: "align",
            attribute: "align",
            reflection: Reflection::String,
        }],
    ),
    (
        "HTMLHRElement",
        &[
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "color",
                attribute: "color",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "noShade",
                attribute: "noshade",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "size",
                attribute: "size",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLPreElement",
        &[Reflected {
            idl: "width",
            attribute: "width",
            reflection: Reflection::Long { default: 0 },
        }],
    ),
    (
        "HTMLQuoteElement",
        &[Reflected {
            idl: "cite",
            attribute: "cite",
            reflection: Reflection::Url,
        }],
    ),
    (
        "HTMLOListElement",
        &[
            Reflected {
                idl: "reversed",
                attribute: "reversed",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "start",
                attribute: "start",
                reflection: Reflection::Long { default: 1 },
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "compact",
                attribute: "compact",
                reflection: Reflection::Boolean,
            },
        ],
    ),
    (
        "HTMLUListElement",
        &[
            Reflected {
                idl: "compact",
                attribute: "compact",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLLIElement",
        &[
            Reflected {
                idl: "value",
                attribute: "value",
                reflection: Reflection::Long { default: 0 },
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLDListElement",
        &[Reflected {
            idl: "compact",
            attribute: "compact",
            reflection: Reflection::Boolean,
        }],
    ),
    (
        "HTMLDivElement",
        &[Reflected {
            idl: "align",
            attribute: "align",
            reflection: Reflection::String,
        }],
    ),
    (
        "HTMLBaseElement",
        &[
            Reflected {
                idl: "href",
                attribute: "href",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "target",
                attribute: "target",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLLinkElement",
        &[
            Reflected {
                idl: "href",
                attribute: "href",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "crossOrigin",
                attribute: "crossorigin",
                reflection: Reflection::Keyword(CROSS_ORIGIN),
            },
            Reflected {
                idl: "rel",
                attribute: "rel",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "as",
                attribute: "as",
                reflection: Reflection::Keyword(AS),
            },
            Reflected {
                idl: "media",
                attribute: "media",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "integrity",
                attribute: "integrity",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "hreflang",
                attribute: "hreflang",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "referrerPolicy",
                attribute: "referrerpolicy",
                reflection: Reflection::Keyword(REFERRER_POLICY),
            },
            Reflected {
                idl: "charset",
                attribute: "charset",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "rev",
                attribute: "rev",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "target",
                attribute: "target",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLMetaElement",
        &[
            Reflected {
                idl: "name",
                attribute: "name",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "httpEquiv",
                attribute: "http-equiv",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "content",
                attribute: "content",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "media",
                attribute: "media",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "scheme",
                attribute: "scheme",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLStyleElement",
        &[
            Reflected {
                idl: "media",
                attribute: "media",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLHtmlElement",
        &[Reflected {
            idl: "version",
            attribute: "version",
            reflection: Reflection::String,
        }],
    ),
    (
        "HTMLScriptElement",
        &[
            Reflected {
                idl: "src",
                attribute: "src",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "noModule",
                attribute: "nomodule",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "charset",
                attribute: "charset",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "defer",
                attribute: "defer",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "crossOrigin",
                attribute: "crossorigin",
                reflection: Reflection::Keyword(CROSS_ORIGIN),
            },
            Reflected {
                idl: "integrity",
                attribute: "integrity",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "event",
                attribute: "event",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "htmlFor",
                attribute: "for",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLSlotElement",
        &[Reflected {
            idl: "name",
            attribute: "name",
            reflection: Reflection::String,
        }],
    ),
    (
        "HTMLModElement",
        &[
            Reflected {
                idl: "cite",
                attribute: "cite",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "dateTime",
                attribute: "datetime",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLDetailsElement",
        &[Reflected {
            idl: "open",
            attribute: "open",
            reflection: Reflection::Boolean,
        }],
    ),
    (
        "HTMLMenuElement",
        &[Reflected {
            idl: "compact",
            attribute: "compact",
            reflection: Reflection::Boolean,
        }],
    ),
    (
        "HTMLDialogElement",
        &[Reflected {
            idl: "open",
            attribute: "open",
            reflection: Reflection::Boolean,
        }],
    ),
    (
        "HTMLMarqueeElement",
        &[
            Reflected {
                idl: "behavior",
                attribute: "behavior",
                reflection: Reflection::Keyword(BEHAVIOR),
            },
            Reflected {
                idl: "bgColor",
                attribute: "bgcolor",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "direction",
                attribute: "direction",
                reflection: Reflection::Keyword(DIRECTION),
            },
            Reflected {
                idl: "height",
                attribute: "height",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "hspace",
                attribute: "hspace",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "scrollAmount",
                attribute: "scrollamount",
                reflection: Reflection::UnsignedLong {
                    default: 6,
                    clamp: None,
                },
            },
            Reflected {
                idl: "scrollDelay",
                attribute: "scrolldelay",
                reflection: Reflection::UnsignedLong {
                    default: 85,
                    clamp: None,
                },
            },
            Reflected {
                idl: "trueSpeed",
                attribute: "truespeed",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "vspace",
                attribute: "vspace",
                reflection: Reflection::UnsignedLong {
                    default: 0,
                    clamp: None,
                },
            },
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLFrameSetElement",
        &[
            Reflected {
                idl: "cols",
                attribute: "cols",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "rows",
                attribute: "rows",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLFrameElement",
        &[
            Reflected {
                idl: "name",
                attribute: "name",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "scrolling",
                attribute: "scrolling",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "src",
                attribute: "src",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "frameBorder",
                attribute: "frameborder",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "longDesc",
                attribute: "longdesc",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "noResize",
                attribute: "noresize",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "marginHeight",
                attribute: "marginheight",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "marginWidth",
                attribute: "marginwidth",
                reflection: Reflection::NullAsEmpty,
            },
        ],
    ),
    (
        "HTMLDirectoryElement",
        &[Reflected {
            idl: "compact",
            attribute: "compact",
            reflection: Reflection::Boolean,
        }],
    ),
    (
        "HTMLFontElement",
        &[
            Reflected {
                idl: "color",
                attribute: "color",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "face",
                attribute: "face",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "size",
                attribute: "size",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLBodyElement",
        &[
            Reflected {
                idl: "text",
                attribute: "text",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "link",
                attribute: "link",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "vLink",
                attribute: "vlink",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "aLink",
                attribute: "alink",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "bgColor",
                attribute: "bgcolor",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "background",
                attribute: "background",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLHeadingElement",
        &[Reflected {
            idl: "align",
            attribute: "align",
            reflection: Reflection::String,
        }],
    ),
    (
        "HTMLTableElement",
        &[
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "border",
                attribute: "border",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "frame",
                attribute: "frame",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "rules",
                attribute: "rules",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "summary",
                attribute: "summary",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "bgColor",
                attribute: "bgcolor",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "cellPadding",
                attribute: "cellpadding",
                reflection: Reflection::NullAsEmpty,
            },
            Reflected {
                idl: "cellSpacing",
                attribute: "cellspacing",
                reflection: Reflection::NullAsEmpty,
            },
        ],
    ),
    (
        "HTMLTableCaptionElement",
        &[Reflected {
            idl: "align",
            attribute: "align",
            reflection: Reflection::String,
        }],
    ),
    (
        "HTMLTableColElement",
        &[
            Reflected {
                idl: "span",
                attribute: "span",
                reflection: Reflection::UnsignedLong {
                    default: 1,
                    clamp: Some((1, 1000)),
                },
            },
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "ch",
                attribute: "char",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "chOff",
                attribute: "charoff",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "vAlign",
                attribute: "valign",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLTableSectionElement",
        &[
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "ch",
                attribute: "char",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "chOff",
                attribute: "charoff",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "vAlign",
                attribute: "valign",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLTableRowElement",
        &[
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "ch",
                attribute: "char",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "chOff",
                attribute: "charoff",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "vAlign",
                attribute: "valign",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "bgColor",
                attribute: "bgcolor",
                reflection: Reflection::NullAsEmpty,
            },
        ],
    ),
    (
        "HTMLTableCellElement",
        &[
            Reflected {
                idl: "colSpan",
                attribute: "colspan",
                reflection: Reflection::UnsignedLong {
                    default: 1,
                    clamp: Some((1, 1000)),
                },
            },
            Reflected {
                idl: "rowSpan",
                attribute: "rowspan",
                reflection: Reflection::UnsignedLong {
                    default: 1,
                    clamp: Some((0, 65534)),
                },
            },
            Reflected {
                idl: "headers",
                attribute: "headers",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "scope",
                attribute: "scope",
                reflection: Reflection::Keyword(SCOPE),
            },
            Reflected {
                idl: "abbr",
                attribute: "abbr",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "align",
                attribute: "align",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "axis",
                attribute: "axis",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "height",
                attribute: "height",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "width",
                attribute: "width",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "ch",
                attribute: "char",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "chOff",
                attribute: "charoff",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "noWrap",
                attribute: "nowrap",
                reflection: Reflection::Boolean,
            },
            Reflected {
                idl: "vAlign",
                attribute: "valign",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "bgColor",
                attribute: "bgcolor",
                reflection: Reflection::NullAsEmpty,
            },
        ],
    ),
    (
        "HTMLAnchorElement",
        &[
            Reflected {
                idl: "target",
                attribute: "target",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "download",
                attribute: "download",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "ping",
                attribute: "ping",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "rel",
                attribute: "rel",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "hreflang",
                attribute: "hreflang",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "type",
                attribute: "type",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "referrerPolicy",
                attribute: "referrerpolicy",
                reflection: Reflection::Keyword(REFERRER_POLICY),
            },
            Reflected {
                idl: "href",
                attribute: "href",
                reflection: Reflection::Url,
            },
            Reflected {
                idl: "coords",
                attribute: "coords",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "charset",
                attribute: "charset",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "name",
                attribute: "name",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "rev",
                attribute: "rev",
                reflection: Reflection::String,
            },
            Reflected {
                idl: "shape",
                attribute: "shape",
                reflection: Reflection::String,
            },
        ],
    ),
    (
        "HTMLDataElement",
        &[Reflected {
            idl: "value",
            attribute: "value",
            reflection: Reflection::String,
        }],
    ),
    (
        "HTMLTimeElement",
        &[Reflected {
            idl: "dateTime",
            attribute: "datetime",
            reflection: Reflection::String,
        }],
    ),
    (
        "HTMLBRElement",
        &[Reflected {
            idl: "clear",
            attribute: "clear",
            reflection: Reflection::String,
        }],
    ),
];
