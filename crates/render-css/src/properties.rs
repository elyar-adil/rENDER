//! Typed parsers for layout-facing computed CSS properties.
//!
//! This module deliberately parses the boundary-preserving serialization from
//! [`super::computed::ComputedValue`]. Percentages and mixed `calc()` trees are
//! retained until layout supplies the appropriate containing-block basis.

use std::error::Error;
use std::fmt;

use cssparser::color::{parse_hash_color, parse_named_color};
use cssparser::{ParseError, ParseErrorKind, Parser, ParserInput, Token};

type CssResult<'i, T> = Result<T, ParseError<'i, ValueError>>;

/// Why a value was rejected. Most rejections carry no more than "invalid", so
/// the unit payload is the default and a grammar that *can* name the offending
/// construct says so, which is what makes a rejection debuggable instead of
/// just a dropped declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValueError {
    detail: Option<String>,
}

impl ValueError {
    fn detail(message: impl Into<String>) -> Self {
        Self {
            detail: Some(message.into()),
        }
    }
}

impl From<()> for ValueError {
    fn from((): ()) -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyParseError {
    property: String,
    line: u32,
    column: u32,
    /// Why the value was rejected, when the grammar can say. Carried so a
    /// diagnostic names the construct rather than only the property.
    detail: Option<String>,
}

impl PropertyParseError {
    #[must_use]
    pub fn property(&self) -> &str {
        &self.property
    }

    /// The reason the value was rejected, if the grammar could name one.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl fmt::Display for PropertyParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid computed value for '{}' at {}:{}",
            self.property, self.line, self.column
        )?;
        if let Some(detail) = &self.detail {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

impl Error for PropertyParseError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LengthUnit {
    Px,
    Cm,
    Mm,
    Q,
    In,
    Pt,
    Pc,
    Em,
    Ex,
    Cap,
    Ch,
    Ic,
    Rem,
    Lh,
    Rlh,
    Vw,
    Vh,
    Vi,
    Vb,
    Vmin,
    Vmax,
    Svw,
    Svh,
    Svi,
    Svb,
    Svmin,
    Svmax,
    Lvw,
    Lvh,
    Lvi,
    Lvb,
    Lvmin,
    Lvmax,
    Dvw,
    Dvh,
    Dvi,
    Dvb,
    Dvmin,
    Dvmax,
    Cqw,
    Cqh,
    Cqi,
    Cqb,
    Cqmin,
    Cqmax,
}

impl LengthUnit {
    fn parse(unit: &str) -> Option<Self> {
        Some(match unit.to_ascii_lowercase().as_str() {
            "px" => Self::Px,
            "cm" => Self::Cm,
            "mm" => Self::Mm,
            "q" => Self::Q,
            "in" => Self::In,
            "pt" => Self::Pt,
            "pc" => Self::Pc,
            "em" => Self::Em,
            "ex" => Self::Ex,
            "cap" => Self::Cap,
            "ch" => Self::Ch,
            "ic" => Self::Ic,
            "rem" => Self::Rem,
            "lh" => Self::Lh,
            "rlh" => Self::Rlh,
            "vw" => Self::Vw,
            "vh" => Self::Vh,
            "vi" => Self::Vi,
            "vb" => Self::Vb,
            "vmin" => Self::Vmin,
            "vmax" => Self::Vmax,
            "svw" => Self::Svw,
            "svh" => Self::Svh,
            "svi" => Self::Svi,
            "svb" => Self::Svb,
            "svmin" => Self::Svmin,
            "svmax" => Self::Svmax,
            "lvw" => Self::Lvw,
            "lvh" => Self::Lvh,
            "lvi" => Self::Lvi,
            "lvb" => Self::Lvb,
            "lvmin" => Self::Lvmin,
            "lvmax" => Self::Lvmax,
            "dvw" => Self::Dvw,
            "dvh" => Self::Dvh,
            "dvi" => Self::Dvi,
            "dvb" => Self::Dvb,
            "dvmin" => Self::Dvmin,
            "dvmax" => Self::Dvmax,
            "cqw" => Self::Cqw,
            "cqh" => Self::Cqh,
            "cqi" => Self::Cqi,
            "cqb" => Self::Cqb,
            "cqmin" => Self::Cqmin,
            "cqmax" => Self::Cqmax,
            _ => return None,
        })
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Px => "px",
            Self::Cm => "cm",
            Self::Mm => "mm",
            Self::Q => "q",
            Self::In => "in",
            Self::Pt => "pt",
            Self::Pc => "pc",
            Self::Em => "em",
            Self::Ex => "ex",
            Self::Cap => "cap",
            Self::Ch => "ch",
            Self::Ic => "ic",
            Self::Rem => "rem",
            Self::Lh => "lh",
            Self::Rlh => "rlh",
            Self::Vw => "vw",
            Self::Vh => "vh",
            Self::Vi => "vi",
            Self::Vb => "vb",
            Self::Vmin => "vmin",
            Self::Vmax => "vmax",
            Self::Svw => "svw",
            Self::Svh => "svh",
            Self::Svi => "svi",
            Self::Svb => "svb",
            Self::Svmin => "svmin",
            Self::Svmax => "svmax",
            Self::Lvw => "lvw",
            Self::Lvh => "lvh",
            Self::Lvi => "lvi",
            Self::Lvb => "lvb",
            Self::Lvmin => "lvmin",
            Self::Lvmax => "lvmax",
            Self::Dvw => "dvw",
            Self::Dvh => "dvh",
            Self::Dvi => "dvi",
            Self::Dvb => "dvb",
            Self::Dvmin => "dvmin",
            Self::Dvmax => "dvmax",
            Self::Cqw => "cqw",
            Self::Cqh => "cqh",
            Self::Cqi => "cqi",
            Self::Cqb => "cqb",
            Self::Cqmin => "cqmin",
            Self::Cqmax => "cqmax",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Length {
    pub value: f32,
    pub unit: LengthUnit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumericType {
    Number,
    Length,
    Percentage,
    LengthPercentage,
}

impl NumericType {
    const fn add(self, other: Self) -> Option<Self> {
        match (self, other) {
            (Self::Number, Self::Number) => Some(Self::Number),
            (Self::Length, Self::Length) => Some(Self::Length),
            (Self::Percentage, Self::Percentage) => Some(Self::Percentage),
            (
                Self::Length | Self::Percentage | Self::LengthPercentage,
                Self::Length | Self::Percentage | Self::LengthPercentage,
            ) => Some(Self::LengthPercentage),
            _ => None,
        }
    }

    const fn is_length_percentage(self) -> bool {
        matches!(
            self,
            Self::Length | Self::Percentage | Self::LengthPercentage
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CalcValue {
    Number(f32),
    Length(Length),
    Percentage(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SumOperator {
    Add,
    Subtract,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProductOperator {
    Multiply,
    Divide,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CalcNode {
    Value(CalcValue),
    Parentheses(Box<Self>),
    Sum {
        first: Box<Self>,
        rest: Vec<(SumOperator, Self)>,
    },
    Product {
        first: Box<Self>,
        rest: Vec<(ProductOperator, Self)>,
    },
    Min(Vec<Self>),
    Max(Vec<Self>),
    Clamp {
        minimum: Box<Self>,
        preferred: Box<Self>,
        maximum: Box<Self>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathFunction {
    Calc,
    Min,
    Max,
    Clamp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Calculation {
    pub function: MathFunction,
    pub value_type: NumericType,
    pub expression: CalcNode,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LengthPercentage {
    Zero,
    Length(Length),
    Percentage(f32),
    Calculation(Calculation),
}

impl LengthPercentage {
    fn definitely_negative(&self) -> bool {
        match self {
            Self::Length(value) => value.value < 0.0,
            Self::Percentage(value) => *value < 0.0,
            Self::Zero | Self::Calculation(_) => false,
        }
    }

    fn is_length_only(&self) -> bool {
        match self {
            Self::Zero | Self::Length(_) => true,
            Self::Percentage(_) => false,
            Self::Calculation(value) => value.value_type == NumericType::Length,
        }
    }

    #[must_use]
    pub fn to_css(&self) -> String {
        match self {
            Self::Zero => "0px".to_owned(),
            Self::Length(value) => format_number_unit(value.value, value.unit.as_str()),
            Self::Percentage(value) => format_number_unit(*value * 100.0, "%"),
            Self::Calculation(value) => value.to_css(),
        }
    }

    /// Resolve this computed `<length-percentage>` at used-value time. The
    /// percentage basis and environment-dependent metrics are supplied by
    /// layout rather than guessed during CSS computation.
    ///
    /// # Errors
    ///
    /// Returns an error when a required containing-block, font, or viewport
    /// metric is unavailable, or when arithmetic cannot produce a finite used
    /// value.
    pub fn resolve(&self, context: &LengthResolutionContext) -> Result<f32, UsedValueError> {
        let value = match self {
            Self::Zero => 0.0,
            Self::Length(value) => resolve_length(*value, context)?,
            Self::Percentage(value) => resolve_percentage(*value, context)?,
            Self::Calculation(value) => resolve_calc_node(&value.expression, context)?,
        };
        if value.is_finite() {
            Ok(value)
        } else {
            Err(UsedValueError::NonFinite)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LengthResolutionContext {
    pub percentage_basis: Option<f32>,
    pub font_size: f32,
    pub root_font_size: f32,
    pub x_height: Option<f32>,
    pub cap_height: Option<f32>,
    pub zero_advance: Option<f32>,
    pub ideographic_advance: Option<f32>,
    pub line_height: f32,
    pub root_line_height: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub small_viewport_width: Option<f32>,
    pub small_viewport_height: Option<f32>,
    pub large_viewport_width: Option<f32>,
    pub large_viewport_height: Option<f32>,
    pub dynamic_viewport_width: Option<f32>,
    pub dynamic_viewport_height: Option<f32>,
    pub container_width: Option<f32>,
    pub container_height: Option<f32>,
    pub container_inline_size: Option<f32>,
    pub container_block_size: Option<f32>,
    pub inline_axis_is_horizontal: bool,
}

impl Default for LengthResolutionContext {
    fn default() -> Self {
        Self {
            percentage_basis: None,
            font_size: 16.0,
            root_font_size: 16.0,
            x_height: None,
            cap_height: None,
            zero_advance: None,
            ideographic_advance: None,
            line_height: 19.2,
            root_line_height: 19.2,
            viewport_width: 0.0,
            viewport_height: 0.0,
            small_viewport_width: None,
            small_viewport_height: None,
            large_viewport_width: None,
            large_viewport_height: None,
            dynamic_viewport_width: None,
            dynamic_viewport_height: None,
            container_width: None,
            container_height: None,
            container_inline_size: None,
            container_block_size: None,
            inline_axis_is_horizontal: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsedValueError {
    MissingPercentageBasis,
    MissingFontMetric(&'static str),
    DivisionByZero,
    NonFinite,
}

impl fmt::Display for UsedValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPercentageBasis => write!(formatter, "percentage basis is unavailable"),
            Self::MissingFontMetric(metric) => {
                write!(formatter, "required font metric '{metric}' is unavailable")
            }
            Self::DivisionByZero => write!(formatter, "division by zero in CSS math"),
            Self::NonFinite => write!(formatter, "CSS math produced a non-finite used value"),
        }
    }
}

impl Error for UsedValueError {}

fn resolve_percentage(
    value: f32,
    context: &LengthResolutionContext,
) -> Result<f32, UsedValueError> {
    context
        .percentage_basis
        .map(|basis| value * basis)
        .ok_or(UsedValueError::MissingPercentageBasis)
}

fn resolve_length(
    length: Length,
    context: &LengthResolutionContext,
) -> Result<f32, UsedValueError> {
    let small_width = context
        .small_viewport_width
        .unwrap_or(context.viewport_width);
    let small_height = context
        .small_viewport_height
        .unwrap_or(context.viewport_height);
    let large_width = context
        .large_viewport_width
        .unwrap_or(context.viewport_width);
    let large_height = context
        .large_viewport_height
        .unwrap_or(context.viewport_height);
    let dynamic_width = context
        .dynamic_viewport_width
        .unwrap_or(context.viewport_width);
    let dynamic_height = context
        .dynamic_viewport_height
        .unwrap_or(context.viewport_height);
    let logical = |width: f32, height: f32| {
        if context.inline_axis_is_horizontal {
            (width, height)
        } else {
            (height, width)
        }
    };
    let (viewport_inline, viewport_block) =
        logical(context.viewport_width, context.viewport_height);
    let (small_inline, small_block) = logical(small_width, small_height);
    let (large_inline, large_block) = logical(large_width, large_height);
    let (dynamic_inline, dynamic_block) = logical(dynamic_width, dynamic_height);
    let container_width = context.container_width.unwrap_or(small_width);
    let container_height = context.container_height.unwrap_or(small_height);
    let container_inline = context.container_inline_size.unwrap_or(small_inline);
    let container_block = context.container_block_size.unwrap_or(small_block);

    let factor = match length.unit {
        LengthUnit::Px => 1.0,
        LengthUnit::Cm => 96.0 / 2.54,
        LengthUnit::Mm => 96.0 / 25.4,
        LengthUnit::Q => 96.0 / 101.6,
        LengthUnit::In => 96.0,
        LengthUnit::Pt => 96.0 / 72.0,
        LengthUnit::Pc => 16.0,
        LengthUnit::Em => context.font_size,
        LengthUnit::Ex => context
            .x_height
            .ok_or(UsedValueError::MissingFontMetric("x-height"))?,
        LengthUnit::Cap => context
            .cap_height
            .ok_or(UsedValueError::MissingFontMetric("cap-height"))?,
        LengthUnit::Ch => context
            .zero_advance
            .ok_or(UsedValueError::MissingFontMetric("zero-advance"))?,
        LengthUnit::Ic => context
            .ideographic_advance
            .ok_or(UsedValueError::MissingFontMetric("ideographic-advance"))?,
        LengthUnit::Rem => context.root_font_size,
        LengthUnit::Lh => context.line_height,
        LengthUnit::Rlh => context.root_line_height,
        LengthUnit::Vw => context.viewport_width / 100.0,
        LengthUnit::Vh => context.viewport_height / 100.0,
        LengthUnit::Vi => viewport_inline / 100.0,
        LengthUnit::Vb => viewport_block / 100.0,
        LengthUnit::Vmin => context.viewport_width.min(context.viewport_height) / 100.0,
        LengthUnit::Vmax => context.viewport_width.max(context.viewport_height) / 100.0,
        LengthUnit::Svw => small_width / 100.0,
        LengthUnit::Svh => small_height / 100.0,
        LengthUnit::Svi => small_inline / 100.0,
        LengthUnit::Svb => small_block / 100.0,
        LengthUnit::Svmin => small_width.min(small_height) / 100.0,
        LengthUnit::Svmax => small_width.max(small_height) / 100.0,
        LengthUnit::Lvw => large_width / 100.0,
        LengthUnit::Lvh => large_height / 100.0,
        LengthUnit::Lvi => large_inline / 100.0,
        LengthUnit::Lvb => large_block / 100.0,
        LengthUnit::Lvmin => large_width.min(large_height) / 100.0,
        LengthUnit::Lvmax => large_width.max(large_height) / 100.0,
        LengthUnit::Dvw => dynamic_width / 100.0,
        LengthUnit::Dvh => dynamic_height / 100.0,
        LengthUnit::Dvi => dynamic_inline / 100.0,
        LengthUnit::Dvb => dynamic_block / 100.0,
        LengthUnit::Dvmin => dynamic_width.min(dynamic_height) / 100.0,
        LengthUnit::Dvmax => dynamic_width.max(dynamic_height) / 100.0,
        LengthUnit::Cqw => container_width / 100.0,
        LengthUnit::Cqh => container_height / 100.0,
        LengthUnit::Cqi => container_inline / 100.0,
        LengthUnit::Cqb => container_block / 100.0,
        LengthUnit::Cqmin => container_inline.min(container_block) / 100.0,
        LengthUnit::Cqmax => container_inline.max(container_block) / 100.0,
    };
    let resolved = length.value * factor;
    if resolved.is_finite() {
        Ok(resolved)
    } else {
        Err(UsedValueError::NonFinite)
    }
}

fn resolve_calc_node(
    node: &CalcNode,
    context: &LengthResolutionContext,
) -> Result<f32, UsedValueError> {
    let value = match node {
        CalcNode::Value(CalcValue::Number(value)) => *value,
        CalcNode::Value(CalcValue::Length(value)) => resolve_length(*value, context)?,
        CalcNode::Value(CalcValue::Percentage(value)) => resolve_percentage(*value, context)?,
        CalcNode::Parentheses(value) => resolve_calc_node(value, context)?,
        CalcNode::Sum { first, rest } => {
            let mut value = resolve_calc_node(first, context)?;
            for (operator, operand) in rest {
                let operand = resolve_calc_node(operand, context)?;
                value = match operator {
                    SumOperator::Add => value + operand,
                    SumOperator::Subtract => value - operand,
                };
            }
            value
        }
        CalcNode::Product { first, rest } => {
            let mut value = resolve_calc_node(first, context)?;
            for (operator, operand) in rest {
                let operand = resolve_calc_node(operand, context)?;
                value = match operator {
                    ProductOperator::Multiply => value * operand,
                    ProductOperator::Divide if operand != 0.0 => value / operand,
                    ProductOperator::Divide => return Err(UsedValueError::DivisionByZero),
                };
            }
            value
        }
        CalcNode::Min(values) => {
            let mut result = f32::INFINITY;
            for value in values {
                result = result.min(resolve_calc_node(value, context)?);
            }
            result
        }
        CalcNode::Max(values) => {
            let mut result = f32::NEG_INFINITY;
            for value in values {
                result = result.max(resolve_calc_node(value, context)?);
            }
            result
        }
        CalcNode::Clamp {
            minimum,
            preferred,
            maximum,
        } => resolve_calc_node(preferred, context)?
            .max(resolve_calc_node(minimum, context)?)
            .min(resolve_calc_node(maximum, context)?),
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err(UsedValueError::NonFinite)
    }
}

impl Calculation {
    #[must_use]
    pub fn to_css(&self) -> String {
        if self.function == MathFunction::Calc {
            format!("calc({})", self.expression.to_css())
        } else {
            self.expression.to_css()
        }
    }
}

impl CalcNode {
    fn to_css(&self) -> String {
        match self {
            Self::Value(CalcValue::Number(value)) => format_number(*value),
            Self::Value(CalcValue::Length(value)) => {
                format_number_unit(value.value, value.unit.as_str())
            }
            Self::Value(CalcValue::Percentage(value)) => format_number_unit(*value * 100.0, "%"),
            Self::Parentheses(value) => format!("({})", value.to_css()),
            Self::Sum { first, rest } => {
                let mut css = first.to_css();
                for (operator, value) in rest {
                    css.push_str(match operator {
                        SumOperator::Add => " + ",
                        SumOperator::Subtract => " - ",
                    });
                    css.push_str(&value.to_css());
                }
                css
            }
            Self::Product { first, rest } => {
                let mut css = first.to_css();
                for (operator, value) in rest {
                    css.push_str(match operator {
                        ProductOperator::Multiply => " * ",
                        ProductOperator::Divide => " / ",
                    });
                    css.push_str(&value.to_css());
                }
                css
            }
            Self::Min(values) => format_function_list("min", values),
            Self::Max(values) => format_function_list("max", values),
            Self::Clamp {
                minimum,
                preferred,
                maximum,
            } => format!(
                "clamp({}, {}, {})",
                minimum.to_css(),
                preferred.to_css(),
                maximum.to_css()
            ),
        }
    }
}

fn format_function_list(name: &str, values: &[CalcNode]) -> String {
    let values = values
        .iter()
        .map(CalcNode::to_css)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({values})")
}

pub(crate) fn format_number(value: f32) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

fn format_number_unit(value: f32, unit: &str) -> String {
    format!("{}{unit}", format_number(value))
}

/// Formats an angle stored in radians as degrees. Converting back from f32
/// radians accumulates round-trip noise (`30deg` would read back as
/// `30.000002`), so the output is snapped to five decimal places: far below
/// any visual precision while keeping canonical values clean.
fn format_degrees(radians: f32) -> String {
    let degrees = radians.to_degrees();
    if degrees.is_finite() {
        format_number((degrees * 1e5).round() / 1e5)
    } else {
        format_number(degrees)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayOutside {
    Block,
    Inline,
    RunIn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayInside {
    Flow,
    FlowRoot,
    Table,
    Flex,
    Grid,
    Ruby,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayBox {
    Contents,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayInternal {
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableRow,
    TableCell,
    TableColumnGroup,
    TableColumn,
    TableCaption,
    RubyBase,
    RubyText,
    RubyBaseContainer,
    RubyTextContainer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Display {
    Box(DisplayBox),
    Internal(DisplayInternal),
    Normal {
        outside: DisplayOutside,
        inside: DisplayInside,
        list_item: bool,
    },
}

macro_rules! keyword_enum {
    ($name:ident { $($variant:ident => $css:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }

        impl $name {
            fn parse(value: &str) -> Option<Self> {
                $(if value.eq_ignore_ascii_case($css) { return Some(Self::$variant); })+
                None
            }

            const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $css),+ }
            }
        }
    };
}

keyword_enum!(Position {
    Static => "static",
    Relative => "relative",
    Absolute => "absolute",
    Sticky => "sticky",
    Fixed => "fixed",
});
keyword_enum!(Float {
    None => "none",
    Left => "left",
    Right => "right",
    InlineStart => "inline-start",
    InlineEnd => "inline-end",
});
keyword_enum!(Clear {
    None => "none",
    Left => "left",
    Right => "right",
    Both => "both",
    InlineStart => "inline-start",
    InlineEnd => "inline-end",
});
keyword_enum!(BoxSizing {
    ContentBox => "content-box",
    BorderBox => "border-box",
});
keyword_enum!(Overflow {
    Visible => "visible",
    Hidden => "hidden",
    Clip => "clip",
    Scroll => "scroll",
    Auto => "auto",
});
keyword_enum!(Visibility {
    Visible => "visible",
    Hidden => "hidden",
    Collapse => "collapse",
});

/// The computed value of `contain` (CSS Containment 2 §2).
///
/// `strict` and `content` are expanded at parse time; the flags then describe
/// exactly which containment types are active. `size` and `inline-size` are
/// mutually exclusive because the grammar selects one of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Containment {
    pub size: bool,
    pub inline_size: bool,
    pub layout: bool,
    pub style: bool,
    pub paint: bool,
}

impl Containment {
    #[must_use]
    pub const fn is_none(self) -> bool {
        !self.size && !self.inline_size && !self.layout && !self.style && !self.paint
    }

    /// Whether intrinsic sizes in both axes are determined as if the box had
    /// no contents. `inline-size` restricts that to the inline axis.
    #[must_use]
    pub const fn contains_size(self) -> bool {
        self.size
    }

    /// Whether the inline-axis intrinsic size is determined as if the box had
    /// no contents.
    #[must_use]
    pub const fn contains_inline_size(self) -> bool {
        self.size || self.inline_size
    }

    /// Whether layout containment is active. This is what makes the principal
    /// box an absolute/fixed positioning containing block and a stacking
    /// context; paint containment has the same containing-block effect but is
    /// tracked separately because its defining effect is clipping.
    #[must_use]
    pub const fn has_layout_containment(self) -> bool {
        self.layout
    }

    #[must_use]
    pub const fn has_paint_containment(self) -> bool {
        self.paint
    }

    /// CSS Containment 2 §3.3 and §3.5: a layout or paint containment box
    /// establishes an independent formatting context and is a containing block
    /// for absolutely and fixed positioned descendants.
    #[must_use]
    pub const fn establishes_independent_formatting_context(self) -> bool {
        self.layout || self.paint
    }

    /// The canonical serialization in grammar order.
    #[must_use]
    pub fn to_css(self) -> String {
        if self.is_none() {
            return "none".to_owned();
        }
        let mut parts = Vec::new();
        if self.size {
            parts.push("size");
        } else if self.inline_size {
            parts.push("inline-size");
        }
        if self.layout {
            parts.push("layout");
        }
        if self.style {
            parts.push("style");
        }
        if self.paint {
            parts.push("paint");
        }
        parts.join(" ")
    }
}

keyword_enum!(ObjectFit {
    Fill => "fill",
    Contain => "contain",
    Cover => "cover",
    None => "none",
    ScaleDown => "scale-down",
});
keyword_enum!(FlexDirection {
    Row => "row",
    RowReverse => "row-reverse",
    Column => "column",
    ColumnReverse => "column-reverse",
});
keyword_enum!(FlexWrap {
    NoWrap => "nowrap",
    Wrap => "wrap",
    WrapReverse => "wrap-reverse",
});
keyword_enum!(JustifyContent {
    Normal => "normal",
    FlexStart => "flex-start",
    FlexEnd => "flex-end",
    Start => "start",
    End => "end",
    Center => "center",
    SpaceBetween => "space-between",
    SpaceAround => "space-around",
    SpaceEvenly => "space-evenly",
});
keyword_enum!(AlignItems {
    Normal => "normal",
    Stretch => "stretch",
    FlexStart => "flex-start",
    FlexEnd => "flex-end",
    Start => "start",
    End => "end",
    Center => "center",
});
keyword_enum!(AlignSelf {
    Auto => "auto",
    Normal => "normal",
    Stretch => "stretch",
    FlexStart => "flex-start",
    FlexEnd => "flex-end",
    Start => "start",
    End => "end",
    Center => "center",
});
keyword_enum!(AlignContent {
    Normal => "normal",
    Stretch => "stretch",
    FlexStart => "flex-start",
    FlexEnd => "flex-end",
    Start => "start",
    End => "end",
    Center => "center",
    SpaceBetween => "space-between",
    SpaceAround => "space-around",
    SpaceEvenly => "space-evenly",
});
keyword_enum!(TextAlign {
    Start => "start",
    End => "end",
    Left => "left",
    Right => "right",
    Center => "center",
    Justify => "justify",
});
// CSS Containment 2 §4: `content-visibility`.
keyword_enum!(ContentVisibility {
    Visible => "visible",
    Auto => "auto",
    Hidden => "hidden",
});

/// One supported color scheme keyword. `<custom-ident>` values are retained
/// for serialization but are not interpreted by this engine, as required by
/// CSS Color Adjustment 1 §2.2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorSchemeName {
    Light,
    Dark,
    Custom(String),
}

impl ColorSchemeName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Custom(value) => value,
        }
    }
}

/// The computed value of CSS Color Adjustment 1 §2.2 `color-scheme`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorSchemeValue {
    Normal,
    Supported {
        schemes: Vec<ColorSchemeName>,
        /// The `only` keyword: the UA may not override the element's scheme.
        /// This engine has no overriding preference yet, but the flag is
        /// retained for serialization and future use.
        only: bool,
    },
}

impl ColorSchemeValue {
    #[must_use]
    pub fn to_css(&self) -> String {
        match self {
            Self::Normal => "normal".to_owned(),
            Self::Supported { schemes, only } => {
                let mut parts = Vec::with_capacity(schemes.len() + usize::from(*only));
                if *only {
                    parts.push("only");
                }
                parts.extend(schemes.iter().map(ColorSchemeName::as_str));
                parts.join(" ")
            }
        }
    }

    /// Resolve the used color scheme for this element.
    ///
    /// `preferred_dark` is the page color scheme's preference. Only `light` and
    /// `dark` are schemes this engine supports; an unknown `<custom-ident>` is
    /// ignored. If neither is present, the UA default (light) is used. `only`
    /// does not change the result without an overriding preference, but is
    /// preserved in the value.
    #[must_use]
    pub fn used_dark(&self, preferred_dark: bool) -> bool {
        let Self::Supported { schemes, .. } = self else {
            return preferred_dark;
        };
        let supports_light = schemes
            .iter()
            .any(|scheme| matches!(scheme, ColorSchemeName::Light));
        let supports_dark = schemes
            .iter()
            .any(|scheme| matches!(scheme, ColorSchemeName::Dark));
        if (preferred_dark && supports_dark) || (!preferred_dark && supports_light) {
            return preferred_dark;
        }
        schemes
            .iter()
            .find_map(|scheme| match scheme {
                ColorSchemeName::Light => Some(false),
                ColorSchemeName::Dark => Some(true),
                ColorSchemeName::Custom(_) => None,
            })
            .unwrap_or(false)
    }
}
// CSS Writing Modes 3 §2.1: the two-value grammar, `Inherited: yes`.
keyword_enum!(Direction {
    Ltr => "ltr",
    Rtl => "rtl",
});
// CSS Text 3 §5.1. Every value is a keyword §5.1 names, and the grammar is a
// single keyword, so the enum is the whole definition.
keyword_enum!(WordBreak {
    Normal => "normal",
    KeepAll => "keep-all",
    BreakAll => "break-all",
    BreakWord => "break-word",
});
// CSS Text 3 §5.2. `strict`, `normal` and `loose` are the three levels of
// kinsoku shori, `auto` is the initial value and "may vary the restrictions
// based on the length of the line", and `anywhere` disregards the
// prohibitions.
keyword_enum!(LineBreak {
    Auto => "auto",
    Loose => "loose",
    Normal => "normal",
    Strict => "strict",
    Anywhere => "anywhere",
});
keyword_enum!(BorderStyle {
    None => "none",
    Hidden => "hidden",
    Dotted => "dotted",
    Dashed => "dashed",
    Solid => "solid",
    Double => "double",
    Groove => "groove",
    Ridge => "ridge",
    Inset => "inset",
    Outset => "outset",
});

// `text-decoration` is a shorthand over four longhands (Text Decoration 4
// §2.6), so all four are registered here and can be validated on their own.
// A doc comment cannot attach to a macro invocation, hence the line comments.
keyword_enum!(TextDecorationStyle {
    Solid => "solid",
    Double => "double",
    Dotted => "dotted",
    Dashed => "dashed",
    Wavy => "wavy",
});
// Text Decoration 4 §2.1. `none` is handled by `TextDecorationLine` rather
// than here, because §2.1 spells it as a value of the whole property and not as
// one of the combinable keywords.
keyword_enum!(TextDecorationLineKeyword {
    Underline => "underline",
    Overline => "overline",
    LineThrough => "line-through",
    Blink => "blink",
    SpellingError => "spelling-error",
    GrammarError => "grammar-error",
});

/// Text Decoration 4 §2.1: `none | [ underline || overline || line-through ||
/// blink ] | spelling-error | grammar-error`. The `||` combinator means the
/// line keywords accumulate, so this is a *set* and `parse_keyword` cannot
/// express it; `underline overline` is a single legal value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextDecorationLine {
    None,
    Lines(Vec<TextDecorationLineKeyword>),
}

impl TextDecorationLine {
    #[must_use]
    pub fn to_css(&self) -> String {
        match self {
            Self::None => "none".to_owned(),
            Self::Lines(lines) => lines
                .iter()
                .map(|line| line.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

/// Text Decoration 4 §2.4: `auto | from-font | <length-percentage> |
/// <line-width>`, where `<line-width>` is `thin | medium | thick | <length>`.
#[derive(Clone, Debug, PartialEq)]
pub enum TextDecorationThickness {
    Auto,
    FromFont,
    Thin,
    Medium,
    Thick,
    Length(LengthPercentage),
}

impl TextDecorationThickness {
    #[must_use]
    pub fn to_css(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::FromFont => "from-font".to_owned(),
            Self::Thin => "thin".to_owned(),
            Self::Medium => "medium".to_owned(),
            Self::Thick => "thick".to_owned(),
            Self::Length(length) => length.to_css(),
        }
    }
}

keyword_enum!(BorderCollapse {
    Separate => "separate",
    Collapse => "collapse",
});
keyword_enum!(ColumnFill {
    Balance => "balance",
    Auto => "auto",
});
keyword_enum!(ColumnSpan {
    None => "none",
    All => "all",
});
keyword_enum!(TableLayout {
    Auto => "auto",
    Fixed => "fixed",
});
keyword_enum!(EmptyCells {
    Show => "show",
    Hide => "hide",
});
// CSS 2.1 §17.4.1 defines `top | bottom`. CSS Writing Modes 3 §6.1 adds the
// logical pair, which is what a page written today actually uses. A doc comment
// cannot attach to a macro invocation, hence the line comment.
keyword_enum!(CaptionSide {
    Top => "top",
    Bottom => "bottom",
    InlineStart => "inline-start",
    InlineEnd => "inline-end",
});

/// CSS 2.1 §17.6.1: the half-open `border-spacing` pair. A single length
/// applies to both axes; the horizontal component is what separates adjacent
/// columns, the vertical component what separates adjacent rows.
#[derive(Clone, Debug, PartialEq)]
pub struct BorderSpacing {
    pub horizontal: LengthPercentage,
    pub vertical: LengthPercentage,
}

/// CSS 2.1 §17.5.3 lists the table-cell alignment keywords, and §10.8.3 gives
/// the same property for inline-level boxes. `Offset` is the `<length>` /
/// `<percentage>` form, resolved against the line box or the cell height.
#[derive(Clone, Debug, PartialEq)]
pub enum VerticalAlign {
    Baseline,
    Sub,
    Super,
    TextTop,
    TextBottom,
    Middle,
    Top,
    Bottom,
    Offset(LengthPercentage),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Size {
    Auto,
    MinContent,
    MaxContent,
    Stretch,
    FitContent(Option<LengthPercentage>),
    LengthPercentage(LengthPercentage),
}

/// A preferred aspect ratio used when one of the box dimensions is auto.
/// `auto` keeps the normal content/replaced-element sizing rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AspectRatio {
    Auto,
    Ratio(f32),
}

impl AspectRatio {
    #[must_use]
    pub fn to_css(self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Ratio(value) => format_number(value),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MaxSize {
    None,
    Size(Size),
}

#[derive(Clone, Debug, PartialEq)]
pub enum AutoLengthPercentage {
    Auto,
    LengthPercentage(LengthPercentage),
}

#[derive(Clone, Debug, PartialEq)]
pub enum BorderWidth {
    Thin,
    Medium,
    Thick,
    Length(LengthPercentage),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FlexBasis {
    Auto,
    Content,
    LengthPercentage(LengthPercentage),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Gap {
    Normal,
    LengthPercentage(LengthPercentage),
}

/// `column-count` (CSS Multi-column Layout §3.1): `auto`, or a positive integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnCount {
    Auto,
    Count(u32),
}

/// `column-width` (CSS Multi-column Layout §3.2): `auto`, or a non-negative
/// length. Percentages are not valid here, so only a length or zero is stored.
#[derive(Clone, Debug, PartialEq)]
pub enum ColumnWidth {
    Auto,
    Length(LengthPercentage),
}

/// A supported explicit grid track list. Integer `repeat()` values are
/// expanded at computed-value time; auto repetition remains symbolic until
/// layout knows the available inline size and item count.
#[derive(Clone, Debug, PartialEq)]
pub enum GridTemplate {
    None,
    Tracks(Vec<GridTrack>),
    AutoRepeat {
        kind: GridAutoRepeat,
        tracks: Vec<GridTrack>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridAutoRepeat {
    Fill,
    Fit,
}

/// One `<grid-line>` component of a `grid-*-start`/`grid-*-end` placement
/// (CSS Grid §8.2). Line numbers are 1-based; negative values count backward
/// from the end of the explicit grid and zero is invalid, per §8.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridLine {
    Auto,
    Line(i32),
    /// `span <integer>`, and bare `span` (a span of one track).
    Span(i32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum GridTrack {
    Breadth(GridTrackBreadth),
    MinMax {
        minimum: LengthPercentage,
        maximum: GridTrackBreadth,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum GridTrackBreadth {
    LengthPercentage(LengthPercentage),
    Fraction(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum CssColor {
    Srgb {
        red: u8,
        green: u8,
        blue: u8,
        alpha: f32,
    },
    CurrentColor,
    Canvas,
    CanvasText,
    /// A `color-mix()` whose operands depend on context (`currentcolor` or a
    /// system color). Absolute mixes resolve to `Srgb` while parsing.
    Mix(Box<super::color::ColorMix>),
    /// A color with one or more missing components (CSS Color 4 §4.4). Its
    /// authored space is retained so a `color-mix()` can carry `none` forward
    /// to an analogous component before conversion.
    Space(Box<super::color::SpaceColor>),
    /// A relative color whose origin depends on context, so it cannot be
    /// evaluated before paint (CSS Color 5 §4).
    Relative(Box<super::color::RelativeColor>),
    /// `light-dark(<color>, <color>)` (CSS Color 5 §7).
    LightDark(Box<super::color::LightDarkColor>),
    /// `alpha(from <color> / <alpha-value>)` (CSS Color 5 §4.10).
    Alpha(Box<super::color::AlphaColor>),
}

impl CssColor {
    #[must_use]
    pub fn to_css(&self) -> String {
        match self {
            Self::Srgb {
                red,
                green,
                blue,
                alpha: 1.0,
            } => format!("rgb({red}, {green}, {blue})"),
            Self::Srgb {
                red,
                green,
                blue,
                alpha,
            } => format!("rgba({red}, {green}, {blue}, {})", format_number(*alpha)),
            Self::CurrentColor => "currentcolor".to_owned(),
            Self::Canvas => "canvas".to_owned(),
            Self::CanvasText => "canvastext".to_owned(),
            Self::Mix(mix) => mix.to_css(),
            Self::Space(color) => color.to_css(),
            Self::Relative(relative) => relative.to_css(),
            Self::LightDark(light_dark) => light_dark.to_css(),
            Self::Alpha(alpha) => alpha.to_css(),
        }
    }
}

/// One function of a CSS `transform` list (CSS Transforms Level 1).
///
/// 3D functions are accepted only when they reduce to a 2D affine
/// contribution: without a 3D pipeline `rotateX`/`rotateY`/`perspective`
/// cannot be honored, so they parse as errors and drop the declaration.
#[derive(Clone, Debug, PartialEq)]
pub enum TransformFunction {
    /// `matrix(a, b, c, d, e, f)` in the spec's argument order.
    Matrix([f32; 6]),
    /// `translate(x, y)` with a missing second argument defaulted to zero.
    Translate(LengthPercentage, LengthPercentage),
    /// `scale(x, y)` with a missing second argument defaulted to the first.
    Scale(f32, f32),
    /// `rotate(angle)` in radians; positive angles rotate clockwise in the
    /// y-down screen coordinate system.
    Rotate(f32),
    /// `skew(ax, ay)` in radians with a missing second argument defaulted
    /// to zero.
    Skew(f32, f32),
}

impl TransformFunction {
    #[must_use]
    pub fn to_css(&self) -> String {
        match self {
            Self::Matrix([a, b, c, d, e, f]) => format!(
                "matrix({}, {}, {}, {}, {}, {})",
                format_number(*a),
                format_number(*b),
                format_number(*c),
                format_number(*d),
                format_number(*e),
                format_number(*f),
            ),
            Self::Translate(x, y) => format!("translate({}, {})", x.to_css(), y.to_css()),
            Self::Scale(x, y) => format!("scale({}, {})", format_number(*x), format_number(*y)),
            Self::Rotate(radians) => format!("rotate({}deg)", format_degrees(*radians)),
            Self::Skew(ax, ay) => format!(
                "skew({}deg, {}deg)",
                format_degrees(*ax),
                format_degrees(*ay),
            ),
        }
    }
}

/// Computed `transform` value; `none` is an empty list.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TransformList(pub Vec<TransformFunction>);

impl TransformList {
    #[must_use]
    pub fn to_css(&self) -> String {
        if self.0.is_empty() {
            return "none".to_owned();
        }
        self.0
            .iter()
            .map(TransformFunction::to_css)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Computed `transform-origin` as an x/y pair. A third z length is accepted
/// by the grammar but ignored by the 2D pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct TransformOrigin(pub LengthPercentage, pub LengthPercentage);

impl TransformOrigin {
    #[must_use]
    pub fn to_css(&self) -> String {
        format!("{} {}", self.0.to_css(), self.1.to_css())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypedPropertyValue {
    Display(Display),
    Position(Position),
    Float(Float),
    Clear(Clear),
    BoxSizing(BoxSizing),
    Overflow(Overflow),
    Visibility(Visibility),
    Contain(Containment),
    ObjectFit(ObjectFit),
    Opacity(f32),
    Size(Size),
    MaxSize(MaxSize),
    Inset(AutoLengthPercentage),
    Margin(AutoLengthPercentage),
    Padding(LengthPercentage),
    BorderWidth(BorderWidth),
    BorderStyle(BorderStyle),
    BorderSpacing(BorderSpacing),
    BorderCollapse(BorderCollapse),
    EmptyCells(EmptyCells),
    TableLayout(TableLayout),
    CaptionSide(CaptionSide),
    VerticalAlign(VerticalAlign),
    TextDecorationLine(TextDecorationLine),
    TextDecorationStyle(TextDecorationStyle),
    TextDecorationThickness(TextDecorationThickness),
    Color(CssColor),
    BackgroundImage(String),
    BackgroundRepeat(String),
    BackgroundPosition(String),
    BackgroundSize(String),
    FlexDirection(FlexDirection),
    FlexWrap(FlexWrap),
    FlexBasis(FlexBasis),
    FlexGrow(f32),
    FlexShrink(f32),
    JustifyContent(JustifyContent),
    AlignItems(AlignItems),
    AlignSelf(AlignSelf),
    AlignContent(AlignContent),
    TextAlign(TextAlign),
    Direction(Direction),
    ContentVisibility(ContentVisibility),
    ColorScheme(ColorSchemeValue),
    WordBreak(WordBreak),
    LineBreak(LineBreak),
    Order(i32),
    Gap(Gap),
    ColumnCount(ColumnCount),
    ColumnWidth(ColumnWidth),
    ColumnFill(ColumnFill),
    ColumnSpan(ColumnSpan),
    GridTemplate(GridTemplate),
    GridLine(GridLine),
    AspectRatio(AspectRatio),
    Transform(TransformList),
    TransformOrigin(TransformOrigin),
}

impl TypedPropertyValue {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Display(_) => "display",
            Self::Position(_) => "position",
            Self::Float(_) => "float",
            Self::Clear(_) => "clear",
            Self::BoxSizing(_) => "box-sizing",
            Self::Overflow(_) => "overflow",
            Self::Visibility(_) => "visibility",
            Self::Contain(_) => "contain",
            Self::ObjectFit(_) => "object-fit",
            Self::Opacity(_) => "opacity",
            Self::Size(_) => "size",
            Self::MaxSize(_) => "max-size",
            Self::Inset(_) => "inset",
            Self::Margin(_) => "margin",
            Self::Padding(_) => "padding",
            Self::BorderWidth(_) => "border-width",
            Self::BorderStyle(_) => "border-style",
            Self::BorderSpacing(_) => "border-spacing",
            Self::BorderCollapse(_) => "border-collapse",
            Self::EmptyCells(_) => "empty-cells",
            Self::TableLayout(_) => "table-layout",
            Self::CaptionSide(_) => "caption-side",
            Self::VerticalAlign(_) => "vertical-align",
            Self::TextDecorationLine(_) => "text-decoration-line",
            Self::TextDecorationStyle(_) => "text-decoration-style",
            Self::TextDecorationThickness(_) => "text-decoration-thickness",
            Self::Color(_) => "color",
            Self::BackgroundImage(_) => "background-image",
            Self::BackgroundRepeat(_) => "background-repeat",
            Self::BackgroundPosition(_) => "background-position",
            Self::BackgroundSize(_) => "background-size",
            Self::FlexDirection(_) => "flex-direction",
            Self::FlexWrap(_) => "flex-wrap",
            Self::FlexBasis(_) => "flex-basis",
            Self::FlexGrow(_) => "flex-grow",
            Self::FlexShrink(_) => "flex-shrink",
            Self::JustifyContent(_) => "justify-content",
            Self::AlignItems(_) => "align-items",
            Self::AlignSelf(_) => "align-self",
            Self::AlignContent(_) => "align-content",
            Self::TextAlign(_) => "text-align",
            Self::Direction(_) => "direction",
            Self::ContentVisibility(_) => "content-visibility",
            Self::ColorScheme(_) => "color-scheme",
            Self::WordBreak(_) => "word-break",
            Self::LineBreak(_) => "line-break",
            Self::Order(_) => "order",
            Self::Gap(_) => "gap",
            Self::ColumnCount(_) => "column-count",
            Self::ColumnWidth(_) => "column-width",
            Self::ColumnFill(_) => "column-fill",
            Self::ColumnSpan(_) => "column-span",
            Self::GridTemplate(_) => "grid-template",
            Self::GridLine(_) => "grid-line",
            Self::AspectRatio(_) => "aspect-ratio",
            Self::Transform(_) => "transform",
            Self::TransformOrigin(_) => "transform-origin",
        }
    }

    #[must_use]
    pub fn to_css(&self) -> String {
        match self {
            Self::Display(value) => value.to_css(),
            Self::Position(value) => value.as_str().to_owned(),
            Self::Float(value) => value.as_str().to_owned(),
            Self::Clear(value) => value.as_str().to_owned(),
            Self::BoxSizing(value) => value.as_str().to_owned(),
            Self::Overflow(value) => value.as_str().to_owned(),
            Self::Visibility(value) => value.as_str().to_owned(),
            Self::Contain(value) => value.to_css(),
            Self::ObjectFit(value) => value.as_str().to_owned(),
            Self::Opacity(value) | Self::FlexGrow(value) | Self::FlexShrink(value) => {
                format_number(*value)
            }
            Self::Size(value) | Self::MaxSize(MaxSize::Size(value)) => value.to_css(),
            Self::MaxSize(MaxSize::None) => "none".to_owned(),
            Self::Inset(value) | Self::Margin(value) => value.to_css(),
            Self::Padding(value) => value.to_css(),
            Self::BorderWidth(value) => value.to_css(),
            Self::BorderStyle(value) => value.as_str().to_owned(),
            Self::BorderSpacing(value) => value.to_css(),
            Self::BorderCollapse(value) => value.as_str().to_owned(),
            Self::EmptyCells(value) => value.as_str().to_owned(),
            Self::TableLayout(value) => value.as_str().to_owned(),
            Self::CaptionSide(value) => value.as_str().to_owned(),
            Self::VerticalAlign(value) => value.to_css(),
            Self::TextDecorationLine(value) => value.to_css(),
            Self::TextDecorationStyle(value) => value.as_str().to_owned(),
            Self::TextDecorationThickness(value) => value.to_css(),
            Self::Color(value) => value.to_css(),
            Self::BackgroundImage(value)
            | Self::BackgroundRepeat(value)
            | Self::BackgroundPosition(value)
            | Self::BackgroundSize(value) => value.clone(),
            Self::FlexDirection(value) => value.as_str().to_owned(),
            Self::FlexWrap(value) => value.as_str().to_owned(),
            Self::FlexBasis(value) => value.to_css(),
            Self::JustifyContent(value) => value.as_str().to_owned(),
            Self::AlignItems(value) => value.as_str().to_owned(),
            Self::AlignSelf(value) => value.as_str().to_owned(),
            Self::AlignContent(value) => value.as_str().to_owned(),
            Self::TextAlign(value) => value.as_str().to_owned(),
            Self::Direction(value) => value.as_str().to_owned(),
            Self::ContentVisibility(value) => value.as_str().to_owned(),
            Self::ColorScheme(value) => value.to_css(),
            Self::WordBreak(value) => value.as_str().to_owned(),
            Self::LineBreak(value) => value.as_str().to_owned(),
            Self::Order(value) => value.to_string(),
            Self::Gap(value) => value.to_css(),
            Self::ColumnCount(value) => value.to_css(),
            Self::ColumnWidth(value) => value.to_css(),
            Self::ColumnFill(value) => value.as_str().to_owned(),
            Self::ColumnSpan(value) => value.as_str().to_owned(),
            Self::GridTemplate(value) => value.to_css(),
            Self::GridLine(value) => value.to_css(),
            Self::AspectRatio(value) => value.to_css(),
            Self::Transform(value) => value.to_css(),
            Self::TransformOrigin(value) => value.to_css(),
        }
    }
}

impl Display {
    fn to_css(&self) -> String {
        match self {
            Self::Box(DisplayBox::Contents) => "contents".to_owned(),
            Self::Box(DisplayBox::None) => "none".to_owned(),
            Self::Internal(value) => value.as_str().to_owned(),
            Self::Normal {
                outside: DisplayOutside::Inline,
                inside: DisplayInside::Flow,
                list_item: false,
            } => "inline".to_owned(),
            Self::Normal {
                outside: DisplayOutside::Block,
                inside: DisplayInside::Flow,
                list_item: false,
            } => "block".to_owned(),
            Self::Normal {
                outside: DisplayOutside::Inline,
                inside: DisplayInside::FlowRoot,
                list_item: false,
            } => "inline-block".to_owned(),
            Self::Normal {
                outside: DisplayOutside::Block,
                inside,
                list_item: false,
            } if *inside != DisplayInside::FlowRoot => inside.as_str().to_owned(),
            Self::Normal {
                outside,
                inside,
                list_item,
            } => {
                let mut parts = vec![outside.as_str(), inside.as_str()];
                if *list_item {
                    parts.push("list-item");
                }
                parts.join(" ")
            }
        }
    }
}

impl DisplayOutside {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Inline => "inline",
            Self::RunIn => "run-in",
        }
    }
}

impl DisplayInside {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::FlowRoot => "flow-root",
            Self::Table => "table",
            Self::Flex => "flex",
            Self::Grid => "grid",
            Self::Ruby => "ruby",
        }
    }
}

impl DisplayInternal {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TableRowGroup => "table-row-group",
            Self::TableHeaderGroup => "table-header-group",
            Self::TableFooterGroup => "table-footer-group",
            Self::TableRow => "table-row",
            Self::TableCell => "table-cell",
            Self::TableColumnGroup => "table-column-group",
            Self::TableColumn => "table-column",
            Self::TableCaption => "table-caption",
            Self::RubyBase => "ruby-base",
            Self::RubyText => "ruby-text",
            Self::RubyBaseContainer => "ruby-base-container",
            Self::RubyTextContainer => "ruby-text-container",
        }
    }
}

impl Size {
    fn to_css(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::MinContent => "min-content".to_owned(),
            Self::MaxContent => "max-content".to_owned(),
            Self::Stretch => "stretch".to_owned(),
            Self::FitContent(None) => "fit-content".to_owned(),
            Self::FitContent(Some(value)) => format!("fit-content({})", value.to_css()),
            Self::LengthPercentage(value) => value.to_css(),
        }
    }
}

impl AutoLengthPercentage {
    fn to_css(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::LengthPercentage(value) => value.to_css(),
        }
    }
}

impl BorderWidth {
    fn to_css(&self) -> String {
        match self {
            Self::Thin => "thin".to_owned(),
            Self::Medium => "medium".to_owned(),
            Self::Thick => "thick".to_owned(),
            Self::Length(value) => value.to_css(),
        }
    }
}

impl BorderSpacing {
    fn to_css(&self) -> String {
        if self.horizontal == self.vertical {
            self.horizontal.to_css()
        } else {
            format!("{} {}", self.horizontal.to_css(), self.vertical.to_css())
        }
    }
}

impl VerticalAlign {
    fn to_css(&self) -> String {
        match self {
            Self::Baseline => "baseline".to_owned(),
            Self::Sub => "sub".to_owned(),
            Self::Super => "super".to_owned(),
            Self::TextTop => "text-top".to_owned(),
            Self::TextBottom => "text-bottom".to_owned(),
            Self::Middle => "middle".to_owned(),
            Self::Top => "top".to_owned(),
            Self::Bottom => "bottom".to_owned(),
            Self::Offset(value) => value.to_css(),
        }
    }
}

impl FlexBasis {
    fn to_css(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Content => "content".to_owned(),
            Self::LengthPercentage(value) => value.to_css(),
        }
    }
}

impl Gap {
    fn to_css(&self) -> String {
        match self {
            Self::Normal => "normal".to_owned(),
            Self::LengthPercentage(value) => value.to_css(),
        }
    }
}

impl ColumnCount {
    fn to_css(self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Count(count) => count.to_string(),
        }
    }
}

impl ColumnWidth {
    fn to_css(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Length(value) => value.to_css(),
        }
    }
}

impl GridTemplate {
    fn to_css(&self) -> String {
        match self {
            Self::None => "none".to_owned(),
            Self::Tracks(tracks) => serialize_grid_tracks(tracks),
            Self::AutoRepeat { kind, tracks } => {
                let keyword = match kind {
                    GridAutoRepeat::Fill => "auto-fill",
                    GridAutoRepeat::Fit => "auto-fit",
                };
                format!("repeat({keyword}, {})", serialize_grid_tracks(tracks))
            }
        }
    }
}

impl GridTrack {
    fn to_css(&self) -> String {
        match self {
            Self::Breadth(value) => value.to_css(),
            Self::MinMax { minimum, maximum } => {
                format!("minmax({}, {})", minimum.to_css(), maximum.to_css())
            }
        }
    }
}

impl GridTrackBreadth {
    fn to_css(&self) -> String {
        match self {
            Self::LengthPercentage(value) => value.to_css(),
            Self::Fraction(value) => format_number_unit(*value, "fr"),
        }
    }
}

impl GridLine {
    fn to_css(self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Line(value) => value.to_string(),
            Self::Span(value) => format!("span {value}"),
        }
    }
}

fn serialize_grid_tracks(tracks: &[GridTrack]) -> String {
    tracks
        .iter()
        .map(GridTrack::to_css)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The properties this crate claims a grammar for, which is what makes
/// `parse_typed_property` return `Some`.
///
/// It is a `const` list rather than a `match` arm so that the set of properties
/// with a grammar is a *declared* thing another module can read. The CSS feature
/// query oracle in [`crate::supports`] has to answer "does this engine support
/// `display: grid`" from the same answer the cascade gives, and a list it can
/// read is what stops the two from drifting the way a second, weaker parse
/// would.
pub const DECLARED_GRAMMARS: &[&str] = &[
    "display",
    "color",
    "background-color",
    "background-image",
    "background-repeat",
    "background-position",
    "background-size",
    "position",
    "float",
    "clear",
    "box-sizing",
    "contain",
    "overflow-x",
    "overflow-y",
    "visibility",
    "object-fit",
    "opacity",
    "width",
    "height",
    "min-width",
    "min-height",
    "max-width",
    "max-height",
    "top",
    "right",
    "bottom",
    "left",
    "inset-inline-start",
    "inset-inline-end",
    "inset-block-start",
    "inset-block-end",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "margin-inline-start",
    "margin-inline-end",
    "margin-block-start",
    "margin-block-end",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "padding-inline-start",
    "padding-inline-end",
    "padding-block-start",
    "padding-block-end",
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
    "border-inline-start-width",
    "border-inline-end-width",
    "border-block-start-width",
    "border-block-end-width",
    "border-top-style",
    "border-right-style",
    "border-bottom-style",
    "border-left-style",
    "border-inline-start-style",
    "border-inline-end-style",
    "border-block-start-style",
    "border-block-end-style",
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
    "border-inline-start-color",
    "border-inline-end-color",
    "border-block-start-color",
    "border-block-end-color",
    "border-spacing",
    "border-collapse",
    "empty-cells",
    "table-layout",
    "caption-side",
    "vertical-align",
    "text-decoration-line",
    "text-decoration-style",
    "text-decoration-color",
    "text-decoration-thickness",
    "flex-direction",
    "flex-wrap",
    "flex-basis",
    "flex-grow",
    "flex-shrink",
    "justify-content",
    "align-items",
    "align-self",
    "align-content",
    "text-align",
    "direction",
    "content-visibility",
    "color-scheme",
    "word-break",
    "line-break",
    "order",
    "row-gap",
    "column-gap",
    "column-count",
    "column-width",
    "column-fill",
    "column-span",
    "grid-template-columns",
    "grid-template-rows",
    "grid-column-start",
    "grid-column-end",
    "grid-row-start",
    "grid-row-end",
    "aspect-ratio",
    "transform",
    "transform-origin",
];

/// Every logical box longhand this engine maps onto a physical longhand, with
/// its physical target under `direction: ltr` and `direction: rtl`.
///
/// The block axis is top/bottom because this engine lays out `horizontal-tb`
/// only. A vertical writing mode would need a `writing-mode` declaration (which
/// this slice does not parse) and would make the block axis the inline axis, so
/// it is deliberately out of scope rather than silently approximated.
pub const LOGICAL_LONGHANDS: &[(&str, &str, &str)] = &[
    ("margin-inline-start", "margin-left", "margin-right"),
    ("margin-inline-end", "margin-right", "margin-left"),
    ("margin-block-start", "margin-top", "margin-top"),
    ("margin-block-end", "margin-bottom", "margin-bottom"),
    ("padding-inline-start", "padding-left", "padding-right"),
    ("padding-inline-end", "padding-right", "padding-left"),
    ("padding-block-start", "padding-top", "padding-top"),
    ("padding-block-end", "padding-bottom", "padding-bottom"),
    ("inset-inline-start", "left", "right"),
    ("inset-inline-end", "right", "left"),
    ("inset-block-start", "top", "top"),
    ("inset-block-end", "bottom", "bottom"),
    (
        "border-inline-start-width",
        "border-left-width",
        "border-right-width",
    ),
    (
        "border-inline-end-width",
        "border-right-width",
        "border-left-width",
    ),
    (
        "border-block-start-width",
        "border-top-width",
        "border-top-width",
    ),
    (
        "border-block-end-width",
        "border-bottom-width",
        "border-bottom-width",
    ),
    (
        "border-inline-start-style",
        "border-left-style",
        "border-right-style",
    ),
    (
        "border-inline-end-style",
        "border-right-style",
        "border-left-style",
    ),
    (
        "border-block-start-style",
        "border-top-style",
        "border-top-style",
    ),
    (
        "border-block-end-style",
        "border-bottom-style",
        "border-bottom-style",
    ),
    (
        "border-inline-start-color",
        "border-left-color",
        "border-right-color",
    ),
    (
        "border-inline-end-color",
        "border-right-color",
        "border-left-color",
    ),
    (
        "border-block-start-color",
        "border-top-color",
        "border-top-color",
    ),
    (
        "border-block-end-color",
        "border-bottom-color",
        "border-bottom-color",
    ),
];

/// The physical longhand a logical box property resolves to for
/// `horizontal-tb` text in `direction`.
///
/// This is the single mapping rule shared by computed-style resolution and the
/// tests that pin it. `direction: rtl` swaps only the inline axis; the block
/// axis remains top/bottom. Returns `None` for every property that is not a
/// supported logical box longhand.
#[must_use]
pub fn physical_longhand_for_logical(property: &str, direction: Direction) -> Option<&'static str> {
    let is_rtl = matches!(direction, Direction::Rtl);
    LOGICAL_LONGHANDS.iter().find_map(|&(logical, ltr, rtl)| {
        (logical == property).then_some(if is_rtl { rtl } else { ltr })
    })
}

/// Parse a supported layout-facing property. `None` means that this slice does
/// not yet claim the property's grammar; `Some(Err(_))` means the property is
/// supported but the value is invalid at computed-value time.
#[must_use]
pub fn parse_typed_property(
    property: &str,
    css: &str,
) -> Option<Result<TypedPropertyValue, PropertyParseError>> {
    if !DECLARED_GRAMMARS.contains(&property) {
        return None;
    }

    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let parsed = parser.parse_entirely(|input| parse_property(property, input));
    Some(parsed.map_err(|error| {
        let detail = match error.kind {
            ParseErrorKind::Custom(value) => value.detail,
            ParseErrorKind::Basic(_) => None,
        };
        PropertyParseError {
            property: property.to_owned(),
            line: error.location.line,
            column: error.location.column,
            detail,
        }
    }))
}

fn parse_property<'i>(
    property: &str,
    input: &mut Parser<'i, '_>,
) -> CssResult<'i, TypedPropertyValue> {
    match property {
        "background-image" => {
            parse_background_image(input).map(TypedPropertyValue::BackgroundImage)
        }
        "background-repeat" => {
            parse_raw_single_layer(input).map(TypedPropertyValue::BackgroundRepeat)
        }
        "background-position" => {
            parse_raw_single_layer(input).map(TypedPropertyValue::BackgroundPosition)
        }
        "background-size" => parse_raw_single_layer(input).map(TypedPropertyValue::BackgroundSize),
        "color"
        | "background-color"
        | "border-top-color"
        | "border-right-color"
        | "border-bottom-color"
        | "border-left-color"
        | "border-inline-start-color"
        | "border-inline-end-color"
        | "border-block-start-color"
        | "border-block-end-color"
        | "text-decoration-color" => parse_color(input).map(TypedPropertyValue::Color),
        "display" => parse_display(input).map(TypedPropertyValue::Display),
        "position" => parse_keyword(input, Position::parse).map(TypedPropertyValue::Position),
        "float" => parse_keyword(input, Float::parse).map(TypedPropertyValue::Float),
        "clear" => parse_keyword(input, Clear::parse).map(TypedPropertyValue::Clear),
        "box-sizing" => parse_keyword(input, BoxSizing::parse).map(TypedPropertyValue::BoxSizing),
        "overflow-x" | "overflow-y" => parse_overflow(input).map(TypedPropertyValue::Overflow),
        "visibility" => parse_keyword(input, Visibility::parse).map(TypedPropertyValue::Visibility),
        "contain" => parse_containment(input).map(TypedPropertyValue::Contain),
        "object-fit" => parse_keyword(input, ObjectFit::parse).map(TypedPropertyValue::ObjectFit),
        "opacity" => parse_opacity(input).map(TypedPropertyValue::Opacity),
        "flex-direction" => {
            parse_keyword(input, FlexDirection::parse).map(TypedPropertyValue::FlexDirection)
        }
        "flex-wrap" => parse_keyword(input, FlexWrap::parse).map(TypedPropertyValue::FlexWrap),
        "flex-basis" => parse_flex_basis(input).map(TypedPropertyValue::FlexBasis),
        "flex-grow" => parse_non_negative_number(input).map(TypedPropertyValue::FlexGrow),
        "flex-shrink" => parse_non_negative_number(input).map(TypedPropertyValue::FlexShrink),
        "justify-content" => {
            parse_keyword(input, JustifyContent::parse).map(TypedPropertyValue::JustifyContent)
        }
        "align-items" => {
            parse_keyword(input, AlignItems::parse).map(TypedPropertyValue::AlignItems)
        }
        "align-self" => parse_keyword(input, AlignSelf::parse).map(TypedPropertyValue::AlignSelf),
        "align-content" => {
            parse_keyword(input, AlignContent::parse).map(TypedPropertyValue::AlignContent)
        }
        "text-align" => parse_keyword(input, TextAlign::parse).map(TypedPropertyValue::TextAlign),
        "direction" => parse_keyword(input, Direction::parse).map(TypedPropertyValue::Direction),
        "content-visibility" => parse_keyword(input, ContentVisibility::parse)
            .map(TypedPropertyValue::ContentVisibility),
        "color-scheme" => parse_color_scheme(input).map(TypedPropertyValue::ColorScheme),
        "word-break" => parse_keyword(input, WordBreak::parse).map(TypedPropertyValue::WordBreak),
        "line-break" => parse_keyword(input, LineBreak::parse).map(TypedPropertyValue::LineBreak),
        "order" => parse_integer(input).map(TypedPropertyValue::Order),
        "row-gap" | "column-gap" => parse_gap(input).map(TypedPropertyValue::Gap),
        "column-count" => parse_column_count(input).map(TypedPropertyValue::ColumnCount),
        "column-width" => parse_column_width(input).map(TypedPropertyValue::ColumnWidth),
        "column-fill" => {
            parse_keyword(input, ColumnFill::parse).map(TypedPropertyValue::ColumnFill)
        }
        "column-span" => {
            parse_keyword(input, ColumnSpan::parse).map(TypedPropertyValue::ColumnSpan)
        }
        "grid-template-columns" | "grid-template-rows" => {
            parse_grid_template(input).map(TypedPropertyValue::GridTemplate)
        }
        "grid-column-start" | "grid-column-end" | "grid-row-start" | "grid-row-end" => {
            parse_grid_line(input).map(TypedPropertyValue::GridLine)
        }
        "aspect-ratio" => parse_aspect_ratio(input).map(TypedPropertyValue::AspectRatio),
        "transform" => parse_transform_list(input).map(TypedPropertyValue::Transform),
        "transform-origin" => {
            parse_transform_origin(input).map(TypedPropertyValue::TransformOrigin)
        }
        "width" | "height" | "min-width" | "min-height" => {
            parse_size(input).map(TypedPropertyValue::Size)
        }
        "max-width" | "max-height" => parse_max_size(input).map(TypedPropertyValue::MaxSize),
        "top" | "right" | "bottom" | "left" => {
            parse_auto_length_percentage(input, false).map(TypedPropertyValue::Inset)
        }
        "inset-inline-start" | "inset-inline-end" | "inset-block-start" | "inset-block-end" => {
            parse_auto_length_percentage(input, false).map(TypedPropertyValue::Inset)
        }
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            parse_auto_length_percentage(input, false).map(TypedPropertyValue::Margin)
        }
        "margin-inline-start" | "margin-inline-end" | "margin-block-start" | "margin-block-end" => {
            parse_auto_length_percentage(input, false).map(TypedPropertyValue::Margin)
        }
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
            parse_length_percentage(input, true).map(TypedPropertyValue::Padding)
        }
        "padding-inline-start"
        | "padding-inline-end"
        | "padding-block-start"
        | "padding-block-end" => {
            parse_length_percentage(input, true).map(TypedPropertyValue::Padding)
        }
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            parse_border_width(input).map(TypedPropertyValue::BorderWidth)
        }
        "border-inline-start-width"
        | "border-inline-end-width"
        | "border-block-start-width"
        | "border-block-end-width" => {
            parse_border_width(input).map(TypedPropertyValue::BorderWidth)
        }
        "border-top-style" | "border-right-style" | "border-bottom-style" | "border-left-style" => {
            parse_keyword(input, BorderStyle::parse).map(TypedPropertyValue::BorderStyle)
        }
        "border-inline-start-style"
        | "border-inline-end-style"
        | "border-block-start-style"
        | "border-block-end-style" => {
            parse_keyword(input, BorderStyle::parse).map(TypedPropertyValue::BorderStyle)
        }
        "border-collapse" => {
            parse_keyword(input, BorderCollapse::parse).map(TypedPropertyValue::BorderCollapse)
        }
        "empty-cells" => {
            parse_keyword(input, EmptyCells::parse).map(TypedPropertyValue::EmptyCells)
        }
        "table-layout" => {
            parse_keyword(input, TableLayout::parse).map(TypedPropertyValue::TableLayout)
        }
        "caption-side" => {
            parse_keyword(input, CaptionSide::parse).map(TypedPropertyValue::CaptionSide)
        }
        "vertical-align" => parse_vertical_align(input).map(TypedPropertyValue::VerticalAlign),
        "border-spacing" => parse_border_spacing(input).map(TypedPropertyValue::BorderSpacing),
        "text-decoration-line" => {
            parse_text_decoration_line(input).map(TypedPropertyValue::TextDecorationLine)
        }
        "text-decoration-style" => parse_keyword(input, TextDecorationStyle::parse)
            .map(TypedPropertyValue::TextDecorationStyle),
        "text-decoration-thickness" => {
            parse_text_decoration_thickness(input).map(TypedPropertyValue::TextDecorationThickness)
        }
        _ => unreachable!("unsupported properties are filtered before parsing"),
    }
}

fn parse_aspect_ratio<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, AspectRatio> {
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("auto"))
        .is_ok()
    {
        // `auto` may be followed by a preferred ratio for replaced elements.
        if input.is_exhausted() {
            return Ok(AspectRatio::Auto);
        }
        let ratio = parse_aspect_ratio_number(input)?;
        return Ok(AspectRatio::Ratio(ratio));
    }
    Ok(AspectRatio::Ratio(parse_aspect_ratio_number(input)?))
}

fn parse_aspect_ratio_number<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    let numerator = input.expect_number()?;
    if !numerator.is_finite() || numerator <= 0.0 {
        return Err(location.new_custom_error(()));
    }
    let denominator = if input
        .try_parse(|candidate| candidate.expect_delim('/'))
        .is_ok()
    {
        let denominator = input.expect_number()?;
        if !denominator.is_finite() || denominator <= 0.0 {
            return Err(location.new_custom_error(()));
        }
        denominator
    } else {
        1.0
    };
    let ratio = numerator / denominator;
    if ratio.is_finite() && ratio > 0.0 {
        Ok(ratio)
    } else {
        Err(location.new_custom_error(()))
    }
}

fn parse_keyword<'i, T>(
    input: &mut Parser<'i, '_>,
    parse: impl FnOnce(&str) -> Option<T>,
) -> CssResult<'i, T> {
    let location = input.current_source_location();
    let ident = input.expect_ident_cloned()?;
    parse(&ident).ok_or_else(|| location.new_custom_error(()))
}

fn parse_overflow<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Overflow> {
    let location = input.current_source_location();
    let ident = input.expect_ident_cloned()?;
    if ident.eq_ignore_ascii_case("overlay") {
        Ok(Overflow::Auto)
    } else {
        Overflow::parse(&ident).ok_or_else(|| location.new_custom_error(()))
    }
}

/// Parse `contain` (CSS Containment 2 §2).
///
/// The grammar is
/// `none | strict | content | [ [ size | inline-size ] || layout || style || paint ]`.
/// The `||` combinator allows the longhands in any order and forbids a
/// repeated component; `size` and `inline-size` are alternatives, not
/// independent flags.
fn parse_containment<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Containment> {
    let location = input.current_source_location();
    let mut idents = vec![input.expect_ident_cloned()?.to_ascii_lowercase()];
    while !input.is_exhausted() {
        idents.push(input.expect_ident_cloned()?.to_ascii_lowercase());
    }
    if idents.len() == 1 {
        match idents[0].as_str() {
            "none" => return Ok(Containment::default()),
            "strict" => {
                return Ok(Containment {
                    size: true,
                    layout: true,
                    style: true,
                    paint: true,
                    ..Containment::default()
                });
            }
            "content" => {
                return Ok(Containment {
                    layout: true,
                    style: true,
                    paint: true,
                    ..Containment::default()
                });
            }
            _ => {}
        }
    }
    let mut value = Containment::default();
    for ident in idents {
        let slot = match ident.as_str() {
            "size" => {
                if value.size || value.inline_size {
                    return Err(location.new_custom_error(()));
                }
                value.size = true;
                continue;
            }
            "inline-size" => {
                if value.size || value.inline_size {
                    return Err(location.new_custom_error(()));
                }
                value.inline_size = true;
                continue;
            }
            "layout" => &mut value.layout,
            "style" => &mut value.style,
            "paint" => &mut value.paint,
            _ => return Err(location.new_custom_error(())),
        };
        if *slot {
            return Err(location.new_custom_error(()));
        }
        *slot = true;
    }
    if value.is_none() {
        Err(location.new_custom_error(()))
    } else {
        Ok(value)
    }
}

/// CSS Color Adjustment 1 §2.2 `color-scheme`.
///
/// `normal` is exclusive; otherwise at least one `<custom-ident>` or
/// `light`/`dark` keyword is required and `only` may appear once. Custom
/// identifiers are retained for serialization but carry no meaning here.
fn parse_color_scheme<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, ColorSchemeValue> {
    let location = input.current_source_location();
    let mut only = false;
    let mut schemes = Vec::new();
    while !input.is_exhausted() {
        let token = input.next()?.clone();
        let Token::Ident(value) = token else {
            return Err(
                location.new_custom_error(ValueError::detail("color-scheme expects identifiers"))
            );
        };
        if value.eq_ignore_ascii_case("normal") {
            if only || !schemes.is_empty() {
                return Err(location.new_custom_error(ValueError::detail(
                    "'normal' cannot be combined with other color schemes",
                )));
            }
            return Ok(ColorSchemeValue::Normal);
        }
        if value.eq_ignore_ascii_case("only") {
            if only {
                return Err(location.new_custom_error(ValueError::detail("'only' may appear once")));
            }
            only = true;
            continue;
        }
        if value.eq_ignore_ascii_case("light") {
            schemes.push(ColorSchemeName::Light);
        } else if value.eq_ignore_ascii_case("dark") {
            schemes.push(ColorSchemeName::Dark);
        } else {
            if [
                "inherit",
                "initial",
                "unset",
                "revert",
                "revert-layer",
                "default",
            ]
            .iter()
            .any(|keyword| value.eq_ignore_ascii_case(keyword))
            {
                return Err(location.new_custom_error(ValueError::detail(
                    "reserved keyword is not a color-scheme custom identifier",
                )));
            }
            schemes.push(ColorSchemeName::Custom(value.to_string()));
        }
    }
    if schemes.is_empty() {
        return Err(location.new_custom_error(ValueError::detail(
            "color-scheme requires at least one color scheme",
        )));
    }
    Ok(ColorSchemeValue::Supported { schemes, only })
}

fn parse_color<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, CssColor> {
    parse_color_with_space(input, false)
}

/// Parse a `color-mix()` operand without collapsing an absolute color to
/// sRGB. Keeping the authored space (and even its explicit components) lets
/// §13.3 decide analogous components and powerlessness before conversion.
fn parse_color_preserving_space<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, CssColor> {
    parse_color_with_space(input, true)
}

fn parse_color_with_space<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    let location = input.current_source_location();
    let token = input.next()?.clone();
    match token {
        Token::IDHash(value) | Token::Hash(value) => {
            let (red, green, blue, alpha) =
                parse_hash_color(value.as_bytes()).map_err(|()| location.new_custom_error(()))?;
            Ok(CssColor::Srgb {
                red,
                green,
                blue,
                alpha,
            })
        }
        Token::Ident(value) if value.eq_ignore_ascii_case("transparent") => Ok(CssColor::Srgb {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0.0,
        }),
        Token::Ident(value) if value.eq_ignore_ascii_case("currentcolor") => {
            Ok(CssColor::CurrentColor)
        }
        Token::Ident(value) if value.eq_ignore_ascii_case("canvas") => Ok(CssColor::Canvas),
        Token::Ident(value) if value.eq_ignore_ascii_case("canvastext") => Ok(CssColor::CanvasText),
        Token::Ident(value) => {
            let (red, green, blue) =
                parse_named_color(&value).map_err(|()| location.new_custom_error(()))?;
            Ok(CssColor::Srgb {
                red,
                green,
                blue,
                alpha: 1.0,
            })
        }
        Token::Function(name)
            if name.eq_ignore_ascii_case("rgb") || name.eq_ignore_ascii_case("rgba") =>
        {
            input.parse_nested_block(|input| parse_rgb_color(input, preserve_space))
        }
        // CSS Color 4 §7 `hsl()` / `hsla()`, including the legacy
        // comma-separated form of CSS Color 3 §4.2.
        Token::Function(name)
            if name.eq_ignore_ascii_case("hsl") || name.eq_ignore_ascii_case("hsla") =>
        {
            input.parse_nested_block(|input| parse_hsl_color(input, preserve_space))
        }
        // CSS Color 4 §10.1 `color()`.
        Token::Function(name) if name.eq_ignore_ascii_case("color") => {
            input.parse_nested_block(|input| parse_color_function(input, preserve_space))
        }
        // CSS Color 4 §8 `hwb()`, §9.3 `lab()`/`lch()`, and §9.4
        // `oklab()`/`oklch()`.
        Token::Function(name) if name.eq_ignore_ascii_case("hwb") => {
            input.parse_nested_block(|input| parse_hwb_color(input, preserve_space))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("lab") => {
            input.parse_nested_block(|input| parse_lab_color(input, preserve_space))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("lch") => {
            input.parse_nested_block(|input| parse_lch_color(input, preserve_space))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("oklab") => {
            input.parse_nested_block(|input| parse_oklab_color(input, preserve_space))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("oklch") => {
            input.parse_nested_block(|input| parse_oklch_color(input, preserve_space))
        }
        // CSS Color 5 §3 `color-mix()`.
        Token::Function(name) if name.eq_ignore_ascii_case("color-mix") => {
            input.parse_nested_block(|input| parse_color_mix(input, preserve_space))
        }
        // CSS Color 5 §7 `light-dark()`.
        Token::Function(name) if name.eq_ignore_ascii_case("light-dark") => {
            input.parse_nested_block(|input| parse_light_dark(input, preserve_space))
        }
        // CSS Color 5 §4.10 `alpha()`.
        Token::Function(name) if name.eq_ignore_ascii_case("alpha") => {
            input.parse_nested_block(|input| parse_alpha_color(input, preserve_space))
        }
        _ => Err(location.new_custom_error(())),
    }
}

/// CSS Color 5 §7: `light-dark(<color>, <color>)`.
fn parse_light_dark<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    let location = input.current_source_location();
    let light = parse_color_with_space(input, preserve_space)?;
    input.expect_comma()?;
    let dark = parse_color_with_space(input, preserve_space)?;
    if !input.is_exhausted() {
        return Err(
            location.new_custom_error(ValueError::detail("light-dark() takes exactly two colors"))
        );
    }
    Ok(CssColor::LightDark(Box::new(
        crate::color::LightDarkColor { light, dark },
    )))
}

/// CSS Color 5 §4.10: `alpha(from <color> / <alpha-value>)`.
fn parse_alpha_color<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    let location = input.current_source_location();
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("from"))
        .is_err()
    {
        return Err(
            location.new_custom_error(ValueError::detail("alpha() requires `from <color>`"))
        );
    }
    let origin = parse_color_with_space(input, true)?;
    input.expect_delim('/')?;
    let alpha = parse_alpha_only(input, &["", "", ""])?;
    if !input.is_exhausted() {
        return Err(location.new_custom_error(ValueError::detail(
            "trailing tokens after the alpha() argument",
        )));
    }
    let value = crate::color::AlphaColor { origin, alpha };
    if let Some(color) =
        crate::color::resolve_absolute_space(&CssColor::Alpha(Box::new(value.clone())))
    {
        if preserve_space || color.has_missing() {
            Ok(CssColor::Space(Box::new(color)))
        } else {
            Ok(crate::color::rgba_to_css(color.to_rgba()))
        }
    } else {
        Ok(CssColor::Alpha(Box::new(value)))
    }
}

/// Parse the alpha-only argument of `alpha()` and of an alpha slot. Component
/// keywords are not part of this grammar: only `alpha` itself, a number,
/// a percentage, `none`, or `calc()`.
fn parse_alpha_only<'i>(
    input: &mut Parser<'i, '_>,
    names: &[&str; 3],
) -> CssResult<'i, crate::color::RelativeChannel> {
    let location = input.current_source_location();
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok(crate::color::RelativeChannel::Number(value)),
        Token::Percentage { unit_value, .. } => {
            Ok(crate::color::RelativeChannel::Percentage(unit_value))
        }
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => {
            Ok(crate::color::RelativeChannel::Missing)
        }
        Token::Ident(value) if value.eq_ignore_ascii_case("alpha") => Ok(
            crate::color::RelativeChannel::Keyword(crate::color::RelativeKeyword::Alpha),
        ),
        Token::Function(name) if name.eq_ignore_ascii_case("calc") => {
            input.parse_nested_block(|input| {
                Ok(crate::color::RelativeChannel::Calc(Box::new(
                    parse_calc_expr(input, names, false)?,
                )))
            })
        }
        _ => Err(location.new_custom_error(())),
    }
}

/// Detect and consume `from <color>` at the start of a relative color
/// function. A missing `from` rewinds the parser and returns `None`.
fn parse_relative_origin<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Option<CssColor>> {
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("from"))
        .is_err()
    {
        return Ok(None);
    }
    Ok(Some(parse_color_with_space(input, true)?))
}

/// Parse the three channels and optional alpha of a relative color, evaluate
/// it when its origin is absolute, and otherwise retain it for paint.
fn parse_relative_color<'i>(
    input: &mut Parser<'i, '_>,
    function: crate::color::RelativeFunction,
    origin: CssColor,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    let location = input.current_source_location();
    let names = function.component_names();
    let components = [
        parse_relative_channel(input, function, &names, 0)?,
        parse_relative_channel(input, function, &names, 1)?,
        parse_relative_channel(input, function, &names, 2)?,
    ];
    let alpha = if input
        .try_parse(|candidate| candidate.expect_delim('/'))
        .is_ok()
    {
        crate::color::RelativeAlpha::Channel(parse_relative_alpha(input, &names)?)
    } else {
        crate::color::RelativeAlpha::Inherit
    };
    if !input.is_exhausted() {
        return Err(location.new_custom_error(ValueError::detail(
            "trailing tokens after the relative color components",
        )));
    }
    let relative = crate::color::RelativeColor {
        function,
        origin,
        components,
        alpha,
    };
    if let Some(color) =
        crate::color::resolve_absolute_space(&CssColor::Relative(Box::new(relative.clone())))
    {
        if preserve_space || color.has_missing() {
            Ok(CssColor::Space(Box::new(color)))
        } else {
            Ok(crate::color::rgba_to_css(color.to_rgba()))
        }
    } else {
        Ok(CssColor::Relative(Box::new(relative)))
    }
}

fn parse_relative_channel<'i>(
    input: &mut Parser<'i, '_>,
    function: crate::color::RelativeFunction,
    names: &[&str; 3],
    slot: usize,
) -> CssResult<'i, crate::color::RelativeChannel> {
    let location = input.current_source_location();
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok(crate::color::RelativeChannel::Number(value)),
        Token::Percentage { unit_value, .. } if !function.is_hue(slot) => {
            Ok(crate::color::RelativeChannel::Percentage(unit_value))
        }
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => {
            Ok(crate::color::RelativeChannel::Missing)
        }
        Token::Ident(value) => {
            let lower = value.to_ascii_lowercase();
            if lower == "alpha" {
                Ok(crate::color::RelativeChannel::Keyword(
                    crate::color::RelativeKeyword::Alpha,
                ))
            } else if let Some(index) = names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&lower))
            {
                Ok(crate::color::RelativeChannel::Keyword(
                    crate::color::RelativeKeyword::Component(u8::try_from(index).unwrap_or(0)),
                ))
            } else {
                Err(location.new_custom_error(ValueError::detail(format!(
                    "'{value}' is not a component of this relative color function"
                ))))
            }
        }
        Token::Function(name) if name.eq_ignore_ascii_case("calc") => {
            input.parse_nested_block(|input| {
                Ok(crate::color::RelativeChannel::Calc(Box::new(
                    parse_calc_expr(input, names, function.is_hue(slot))?,
                )))
            })
        }
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_relative_alpha<'i>(
    input: &mut Parser<'i, '_>,
    names: &[&str; 3],
) -> CssResult<'i, crate::color::RelativeChannel> {
    let location = input.current_source_location();
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok(crate::color::RelativeChannel::Number(value)),
        Token::Percentage { unit_value, .. } => {
            Ok(crate::color::RelativeChannel::Percentage(unit_value))
        }
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => {
            Ok(crate::color::RelativeChannel::Missing)
        }
        Token::Ident(value) => {
            let lower = value.to_ascii_lowercase();
            if lower == "alpha" {
                Ok(crate::color::RelativeChannel::Keyword(
                    crate::color::RelativeKeyword::Alpha,
                ))
            } else if let Some(index) = names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&lower))
            {
                Ok(crate::color::RelativeChannel::Keyword(
                    crate::color::RelativeKeyword::Component(u8::try_from(index).unwrap_or(0)),
                ))
            } else {
                Err(location.new_custom_error(()))
            }
        }
        Token::Function(name) if name.eq_ignore_ascii_case("calc") => {
            input.parse_nested_block(|input| {
                Ok(crate::color::RelativeChannel::Calc(Box::new(
                    parse_calc_expr(input, names, false)?,
                )))
            })
        }
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_calc_expr<'i>(
    input: &mut Parser<'i, '_>,
    names: &[&str; 3],
    hue: bool,
) -> CssResult<'i, crate::color::CalcExpr> {
    let mut expression = parse_calc_product(input, names, hue)?;
    loop {
        let state = input.state();
        let operator = if input
            .try_parse(|candidate| candidate.expect_delim('+'))
            .is_ok()
        {
            Some(true)
        } else if input
            .try_parse(|candidate| candidate.expect_delim('-'))
            .is_ok()
        {
            Some(false)
        } else {
            None
        };
        let Some(add) = operator else {
            input.reset(&state);
            break;
        };
        let right = parse_calc_product(input, names, hue)?;
        expression = if add {
            crate::color::CalcExpr::Add(Box::new(expression), Box::new(right))
        } else {
            crate::color::CalcExpr::Sub(Box::new(expression), Box::new(right))
        };
    }
    Ok(expression)
}

fn parse_calc_product<'i>(
    input: &mut Parser<'i, '_>,
    names: &[&str; 3],
    hue: bool,
) -> CssResult<'i, crate::color::CalcExpr> {
    let mut expression = parse_calc_factor(input, names, hue)?;
    loop {
        let state = input.state();
        let operator = if input
            .try_parse(|candidate| candidate.expect_delim('*'))
            .is_ok()
        {
            Some(true)
        } else if input
            .try_parse(|candidate| candidate.expect_delim('/'))
            .is_ok()
        {
            Some(false)
        } else {
            None
        };
        let Some(multiply) = operator else {
            input.reset(&state);
            break;
        };
        let right = parse_calc_factor(input, names, hue)?;
        expression = if multiply {
            crate::color::CalcExpr::Mul(Box::new(expression), Box::new(right))
        } else {
            crate::color::CalcExpr::Div(Box::new(expression), Box::new(right))
        };
    }
    Ok(expression)
}

fn parse_calc_factor<'i>(
    input: &mut Parser<'i, '_>,
    names: &[&str; 3],
    hue: bool,
) -> CssResult<'i, crate::color::CalcExpr> {
    let location = input.current_source_location();
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok(crate::color::CalcExpr::Number(value)),
        Token::Percentage { unit_value, .. } if !hue => {
            Ok(crate::color::CalcExpr::Percentage(unit_value))
        }
        Token::Ident(value) => {
            let lower = value.to_ascii_lowercase();
            if lower == "alpha" {
                Ok(crate::color::CalcExpr::Keyword(
                    crate::color::RelativeKeyword::Alpha,
                ))
            } else if let Some(index) = names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&lower))
            {
                Ok(crate::color::CalcExpr::Keyword(
                    crate::color::RelativeKeyword::Component(u8::try_from(index).unwrap_or(0)),
                ))
            } else {
                Err(location.new_custom_error(()))
            }
        }
        Token::ParenthesisBlock => {
            input.parse_nested_block(|input| parse_calc_expr(input, names, hue))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("calc") => {
            input.parse_nested_block(|input| parse_calc_expr(input, names, hue))
        }
        Token::Delim('-') => Ok(crate::color::CalcExpr::Negate(Box::new(parse_calc_factor(
            input, names, hue,
        )?))),
        Token::Delim('+') => parse_calc_factor(input, names, hue),
        _ => Err(location.new_custom_error(())),
    }
}

/// CSS Color 4 §10.1: `color( <colorspace-params> [ / <alpha-value> ]? )`.
///
/// The engine paints in sRGB, so a value in another space is converted with
/// the algorithm of §10.12: undo the source gamma, go to CIE XYZ (D65, no
/// chromatic adaptation, because sRGB and Display P3 share the D65 white
/// point), then apply the destination transfer function. The matrices are the
/// ones in §19's sample code.
///
/// Only spaces listed by [`crate::color::ColorSpace::is_predefined_space`] are
/// accepted. A space this engine cannot represent is **rejected with a
/// diagnostic naming it**, never approximated: a silently wrong colour is
/// worse than a missing declaration, because the page still lays out and only
/// the paint is wrong.
fn parse_color_function<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    let origin = parse_relative_origin(input)?;
    let location = input.current_source_location();
    let space = input.expect_ident_cloned()?.to_ascii_lowercase();
    if let Some(origin) = origin {
        let Some(space) = crate::color::ColorSpace::from_ident(&space) else {
            return Err(location.new_custom_error(ValueError::detail(format!(
                "the '{space}' color space is not implemented"
            ))));
        };
        if !space.is_predefined_space() {
            return Err(location.new_custom_error(ValueError::detail(format!(
                "the '{}' color space is not a color() space; use its dedicated function",
                space.name()
            ))));
        }
        return parse_relative_color(
            input,
            crate::color::RelativeFunction::Color(space),
            origin,
            preserve_space,
        );
    }
    let red = parse_color_component(input)?;
    let green = parse_color_component(input)?;
    let blue = parse_color_component(input)?;
    let alpha = parse_modern_alpha(input)?;
    if !input.is_exhausted() {
        return Err(location.new_custom_error(ValueError::detail(
            "trailing tokens after the color() components",
        )));
    }
    let Some(space) = crate::color::ColorSpace::from_ident(&space) else {
        return Err(location.new_custom_error(ValueError::detail(format!(
            "the '{space}' color space is not implemented"
        ))));
    };
    if !space.is_predefined_space() {
        return Err(location.new_custom_error(ValueError::detail(format!(
            "the '{}' color space is not a color() space; use its dedicated function",
            space.name()
        ))));
    }
    Ok(space_color_or_srgb(
        space,
        [red, green, blue],
        alpha,
        preserve_space,
    ))
}

/// CSS Color 4 §4.4: keep a color that has missing components in its authored
/// space so interpolation can carry them forward; otherwise compute it to the
/// engine's sRGB paint value exactly as before.
fn space_color_or_srgb(
    space: crate::color::ColorSpace,
    components: [Option<f32>; 3],
    alpha: Option<f32>,
    preserve_space: bool,
) -> CssColor {
    if preserve_space || components.iter().any(Option::is_none) || alpha.is_none() {
        return CssColor::Space(Box::new(crate::color::SpaceColor::new(
            space, components, alpha,
        )));
    }
    let mut srgb =
        crate::color::space_to_srgb(space, components.map(|component| component.unwrap_or(0.0)));
    srgb.alpha = alpha.unwrap_or(1.0).clamp(0.0, 1.0);
    crate::color::rgba_to_css(srgb)
}

/// CSS Color 4 §4.1.1: a `<predefined-rgb>` component is a `<number>` in
/// `0.0..1.0` or a `<percentage>` in `0%..100%`; `none` is a missing component
/// (§4.4) that here retains its missing state for interpolation.
fn parse_color_component<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Option<f32>> {
    let location = input.current_source_location();
    let value = match input.next()?.clone() {
        Token::Number { value, .. } => Some(value),
        // `unit_value` is already normalized to `0.0..=1.0`.
        Token::Percentage { unit_value, .. } => Some(unit_value),
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => None,
        _ => return Err(location.new_custom_error(())),
    };
    if value.is_none_or(f32::is_finite) {
        Ok(value)
    } else {
        Err(location.new_custom_error(()))
    }
}

/// Parse `hwb()` (CSS Color 4 §8) and compute it to sRGB.
fn parse_hwb_color<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    if let Some(origin) = parse_relative_origin(input)? {
        return parse_relative_color(
            input,
            crate::color::RelativeFunction::Hwb,
            origin,
            preserve_space,
        );
    }
    let location = input.current_source_location();
    let hue = parse_hue_or_none(input)?;
    let white = parse_percentage_or_none(input)?;
    let black = parse_percentage_or_none(input)?;
    let alpha = parse_modern_alpha(input)?;
    if !input.is_exhausted() {
        return Err(location.new_custom_error(()));
    }
    let components = [
        hue.map(|value| value.rem_euclid(360.0)),
        white.map(|value| value.clamp(0.0, 1.0)),
        black.map(|value| value.clamp(0.0, 1.0)),
    ];
    Ok(space_color_or_srgb(
        crate::color::ColorSpace::Hwb,
        components,
        alpha,
        preserve_space,
    ))
}

/// Parse `lab()` (CSS Color 4 §9.3) and compute it to sRGB.
fn parse_lab_color<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    if let Some(origin) = parse_relative_origin(input)? {
        return parse_relative_color(
            input,
            crate::color::RelativeFunction::Lab,
            origin,
            preserve_space,
        );
    }
    let location = input.current_source_location();
    let lightness = parse_number_or_percentage(input, 100.0)?.map(|value| value.clamp(0.0, 100.0));
    let a = parse_number_or_percentage(input, 125.0)?;
    let b = parse_number_or_percentage(input, 125.0)?;
    let alpha = parse_modern_alpha(input)?;
    if !input.is_exhausted() {
        return Err(location.new_custom_error(()));
    }
    Ok(space_color_or_srgb(
        crate::color::ColorSpace::Lab,
        [lightness, a, b],
        alpha,
        preserve_space,
    ))
}

/// Parse `lch()` (CSS Color 4 §9.3) and compute it to sRGB.
fn parse_lch_color<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    if let Some(origin) = parse_relative_origin(input)? {
        return parse_relative_color(
            input,
            crate::color::RelativeFunction::Lch,
            origin,
            preserve_space,
        );
    }
    let location = input.current_source_location();
    let lightness = parse_number_or_percentage(input, 100.0)?.map(|value| value.clamp(0.0, 100.0));
    let chroma = parse_number_or_percentage(input, 150.0)?.map(|value| value.max(0.0));
    let hue = parse_hue_or_none(input)?.map(|value| value.rem_euclid(360.0));
    let alpha = parse_modern_alpha(input)?;
    if !input.is_exhausted() {
        return Err(location.new_custom_error(()));
    }
    Ok(space_color_or_srgb(
        crate::color::ColorSpace::Lch,
        [lightness, chroma, hue],
        alpha,
        preserve_space,
    ))
}

/// Parse `oklab()` (CSS Color 4 §9.4) and compute it to sRGB.
fn parse_oklab_color<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    if let Some(origin) = parse_relative_origin(input)? {
        return parse_relative_color(
            input,
            crate::color::RelativeFunction::Oklab,
            origin,
            preserve_space,
        );
    }
    let location = input.current_source_location();
    let lightness = parse_number_or_percentage(input, 1.0)?.map(|value| value.clamp(0.0, 1.0));
    let a = parse_number_or_percentage(input, 0.4)?;
    let b = parse_number_or_percentage(input, 0.4)?;
    let alpha = parse_modern_alpha(input)?;
    if !input.is_exhausted() {
        return Err(location.new_custom_error(()));
    }
    Ok(space_color_or_srgb(
        crate::color::ColorSpace::Oklab,
        [lightness, a, b],
        alpha,
        preserve_space,
    ))
}

/// Parse `oklch()` (CSS Color 4 §9.4) and compute it to sRGB.
fn parse_oklch_color<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    if let Some(origin) = parse_relative_origin(input)? {
        return parse_relative_color(
            input,
            crate::color::RelativeFunction::Oklch,
            origin,
            preserve_space,
        );
    }
    let location = input.current_source_location();
    let lightness = parse_number_or_percentage(input, 1.0)?.map(|value| value.clamp(0.0, 1.0));
    let chroma = parse_number_or_percentage(input, 0.4)?.map(|value| value.max(0.0));
    let hue = parse_hue_or_none(input)?.map(|value| value.rem_euclid(360.0));
    let alpha = parse_modern_alpha(input)?;
    if !input.is_exhausted() {
        return Err(location.new_custom_error(()));
    }
    Ok(space_color_or_srgb(
        crate::color::ColorSpace::Oklch,
        [lightness, chroma, hue],
        alpha,
        preserve_space,
    ))
}

/// Parse `color-mix()` (CSS Color 5 §3).
///
/// The grammar is
/// `color-mix( <color-interpolation-method> , <color> [ <percentage> ]? , <color> [ <percentage> ]? )`.
/// A mix of two absolute colors resolves at parse time; one naming
/// `currentcolor` or a system color stays a [`CssColor::Mix`] until paint.
fn parse_color_mix<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    let location = input.current_source_location();
    let keyword = input.expect_ident_cloned()?;
    if !keyword.eq_ignore_ascii_case("in") {
        return Err(location.new_custom_error(ValueError::detail(
            "color-mix() requires an `in <color-space>` interpolation method",
        )));
    }
    let space_name = input.expect_ident_cloned()?;
    let Some(space) = crate::color::ColorSpace::from_ident(&space_name) else {
        return Err(location.new_custom_error(ValueError::detail(format!(
            "'{space_name}' is not a color space"
        ))));
    };
    let hue = if space.is_polar() {
        match input.try_parse(|candidate| candidate.expect_ident_cloned()) {
            Ok(name) => crate::color::HueMethod::from_ident(&name).ok_or_else(|| {
                location.new_custom_error(ValueError::detail(format!(
                    "'{name}' is not a hue interpolation method"
                )))
            })?,
            Err(_) => crate::color::HueMethod::Shorter,
        }
    } else {
        crate::color::HueMethod::Shorter
    };
    input.expect_comma()?;
    let (left, left_percent) = parse_color_mix_operand(input)?;
    input.expect_comma()?;
    let (right, right_percent) = parse_color_mix_operand(input)?;
    if !input.is_exhausted() {
        return Err(location.new_custom_error(()));
    }
    let (left_weight, right_weight, alpha_multiplier) =
        color_mix_weights(left_percent, right_percent);
    let mix = CssColor::Mix(Box::new(crate::color::ColorMix {
        space,
        hue,
        left,
        right,
        left_percent,
        right_percent,
        left_weight,
        right_weight,
        alpha_multiplier,
    }));
    if let Some(color) = crate::color::resolve_absolute_space(&mix) {
        if preserve_space || color.has_missing() {
            Ok(CssColor::Space(Box::new(color)))
        } else {
            Ok(crate::color::rgba_to_css(color.to_rgba()))
        }
    } else {
        Ok(mix)
    }
}

fn parse_color_mix_operand<'i>(
    input: &mut Parser<'i, '_>,
) -> CssResult<'i, (CssColor, Option<f32>)> {
    let color = parse_color_preserving_space(input)?;
    let percent = input.try_parse(parse_color_mix_percentage).ok();
    Ok((color, percent))
}

fn parse_color_mix_percentage<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    match input.next()?.clone() {
        Token::Percentage { unit_value, .. } if (0.0..=1.0).contains(&unit_value) => {
            Ok(unit_value * 100.0)
        }
        _ => Err(location.new_custom_error(())),
    }
}

/// CSS Color 5 §3.3: turn the declared percentages into normalized weights.
///
/// When both percentages are present and sum below 100%, the two colors keep
/// their relative ratio and the result's alpha is multiplied by the sum. When
/// they sum above 100%, the weights are normalized.
fn color_mix_weights(left: Option<f32>, right: Option<f32>) -> (f32, f32, f32) {
    match (left, right) {
        (None, None) => (0.5, 0.5, 1.0),
        (Some(left), None) => (left / 100.0, 1.0 - left / 100.0, 1.0),
        (None, Some(right)) => (1.0 - right / 100.0, right / 100.0, 1.0),
        (Some(left), Some(right)) => {
            let sum = left + right;
            if sum <= 0.0 {
                (0.0, 0.0, 0.0)
            } else if sum > 100.0 {
                (left / sum, right / sum, 1.0)
            } else {
                (left / sum, right / sum, sum / 100.0)
            }
        }
    }
}

/// A `<percentage>` component that may be the missing-component keyword
/// `none` (CSS Color 4 §4.4).
fn parse_percentage_or_none<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Option<f32>> {
    let location = input.current_source_location();
    let value = match input.next()?.clone() {
        Token::Percentage { unit_value, .. } => Some(unit_value),
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => None,
        _ => return Err(location.new_custom_error(())),
    };
    if value.is_none_or(f32::is_finite) {
        Ok(value)
    } else {
        Err(location.new_custom_error(()))
    }
}

/// A `<number>` or `<percentage>` component where `100%` scales to
/// `percentage_scale`; `none` stays missing.
fn parse_number_or_percentage<'i>(
    input: &mut Parser<'i, '_>,
    percentage_scale: f32,
) -> CssResult<'i, Option<f32>> {
    let location = input.current_source_location();
    let value = match input.next()?.clone() {
        Token::Number { value, .. } => Some(value),
        Token::Percentage { unit_value, .. } => Some(unit_value * percentage_scale),
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => None,
        _ => return Err(location.new_custom_error(())),
    };
    if value.is_none_or(f32::is_finite) {
        Ok(value)
    } else {
        Err(location.new_custom_error(()))
    }
}

fn parse_hue_or_none<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Option<f32>> {
    if let Ok(hue) = input.try_parse(parse_hue) {
        return Ok(Some(hue));
    }
    let location = input.current_source_location();
    match input.next()?.clone() {
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => Ok(None),
        _ => Err(location.new_custom_error(())),
    }
}

/// The modern `/ <alpha-value>` term, where `none` is a missing alpha (§4.2).
fn parse_modern_alpha<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Option<f32>> {
    if input
        .try_parse(|candidate| candidate.expect_delim('/'))
        .is_err()
    {
        return Ok(Some(1.0));
    }
    let location = input.current_source_location();
    let value = match input.next()?.clone() {
        Token::Number { value, .. } => Some(value.clamp(0.0, 1.0)),
        Token::Percentage { unit_value, .. } => Some(unit_value.clamp(0.0, 1.0)),
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => None,
        _ => return Err(location.new_custom_error(())),
    };
    if value.is_none_or(f32::is_finite) {
        Ok(value)
    } else {
        Err(location.new_custom_error(()))
    }
}

/// CSS Backgrounds 3 §3.2: `<bg-image>#`, a comma-separated list. Real sheets
/// routinely ship `background-image: url(a.svg), none`, so only the first layer
/// may not be enough to keep the declaration.
fn parse_background_image<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, String> {
    let start = input.position();
    let mut layers = Vec::new();
    loop {
        layers.push(parse_single_background_image(input)?);
        if input.try_parse(Parser::expect_comma).is_err() {
            break;
        }
        // A trailing comma leaves an empty final layer, which is invalid.
        if input.is_exhausted() {
            return Err(input.new_custom_error(()));
        }
    }
    Ok(input.slice_from(start).trim().to_owned())
}

fn parse_single_background_image<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, String> {
    let location = input.current_source_location();
    let start = input.position();
    let token = input.next()?.clone();
    match token {
        Token::UnquotedUrl(url) => Ok(format!("url({url})")),
        Token::Function(name) if name.eq_ignore_ascii_case("url") => {
            input.parse_nested_block(|nested| {
                let value = nested.next()?.clone();
                match value {
                    Token::UnquotedUrl(url) | Token::Ident(url) | Token::QuotedString(url) => {
                        Ok(format!("url({url})"))
                    }
                    _ => Err(location.new_custom_error(())),
                }
            })
        }
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => Ok("none".to_owned()),
        // Every other image function is kept verbatim; painting it is another
        // subsystem's job, but dropping the whole declaration is not.
        Token::Function(_) => {
            input.parse_nested_block(|nested| {
                while nested.next().is_ok() {}
                Ok(())
            })?;
            Ok(input.slice_from(start).trim().to_owned())
        }
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_raw_single_layer<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, String> {
    let start = input.position();
    while input.next_including_whitespace_and_comments().is_ok() {}
    let value = input.slice_from(start).trim();
    if value.is_empty() || value.contains(',') {
        Err(input.new_custom_error(()))
    } else {
        Ok(value.to_ascii_lowercase())
    }
}

fn parse_rgb_color<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    if let Some(origin) = parse_relative_origin(input)? {
        return parse_relative_color(
            input,
            crate::color::RelativeFunction::Rgb,
            origin,
            preserve_space,
        );
    }
    let location = input.current_source_location();
    let first = parse_rgb_component(input)?;
    let comma_syntax = input.try_parse(Parser::expect_comma).is_ok();
    let second = parse_rgb_component(input)?;
    if comma_syntax {
        input.expect_comma()?;
    }
    let third = parse_rgb_component(input)?;
    let alpha = if comma_syntax {
        if input.try_parse(Parser::expect_comma).is_ok() {
            parse_alpha_component(input, false)?
        } else {
            Some(1.0)
        }
    } else if input
        .try_parse(|candidate| candidate.expect_delim('/'))
        .is_ok()
    {
        parse_alpha_component(input, true)?
    } else {
        Some(1.0)
    };
    if comma_syntax && (first.is_none() || second.is_none() || third.is_none() || alpha.is_none()) {
        // CSS Color 4 §4.1.2: legacy comma syntax never accepts `none`.
        return Err(location.new_custom_error(()));
    }
    let components = [first, second, third]
        .map(|component| component.map(|value| value.clamp(0.0, 255.0) / 255.0));
    if preserve_space || components.iter().any(Option::is_none) || alpha.is_none() {
        return Ok(CssColor::Space(Box::new(crate::color::SpaceColor::new(
            crate::color::ColorSpace::Srgb,
            components,
            alpha,
        ))));
    }
    let values = components.map(|component| component.unwrap_or(0.0));
    Ok(CssColor::Srgb {
        red: rounded_rgb_channel(values[0] * 255.0),
        green: rounded_rgb_channel(values[1] * 255.0),
        blue: rounded_rgb_channel(values[2] * 255.0),
        alpha: alpha.unwrap_or(1.0).clamp(0.0, 1.0),
    })
}

fn rounded_rgb_channel(value: f32) -> u8 {
    debug_assert!(value.is_finite());
    // The preceding finite check and this clamp establish the complete `u8`
    // range before conversion. Rust's float-to-integer cast is saturating;
    // retaining the explicit clamp also documents CSS Color's clamping step.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        value.round().clamp(0.0, 255.0) as u8
    }
}

fn parse_rgb_component<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Option<f32>> {
    let location = input.current_source_location();
    let value = match input.next()?.clone() {
        Token::Number { value, .. } => Some(value),
        Token::Percentage { unit_value, .. } => Some(unit_value * 255.0),
        Token::Ident(value) if value.eq_ignore_ascii_case("none") => None,
        _ => return Err(location.new_custom_error(())),
    };
    if value.is_none_or(f32::is_finite) {
        Ok(value)
    } else {
        Err(location.new_custom_error(()))
    }
}

fn parse_alpha_component<'i>(
    input: &mut Parser<'i, '_>,
    allow_none: bool,
) -> CssResult<'i, Option<f32>> {
    let location = input.current_source_location();
    let value = match input.next()?.clone() {
        Token::Number { value, .. } => Some(value.clamp(0.0, 1.0)),
        Token::Percentage { unit_value, .. } => Some(unit_value.clamp(0.0, 1.0)),
        Token::Ident(value) if allow_none && value.eq_ignore_ascii_case("none") => None,
        _ => return Err(location.new_custom_error(())),
    };
    if value.is_none_or(f32::is_finite) {
        Ok(value)
    } else {
        Err(location.new_custom_error(()))
    }
}

/// Parse `hsl()` / `hsla()` and resolve it to sRGB.
///
/// CSS Color 4 §7 defines the modern space-separated form with a `/` alpha
/// separator; CSS Color 3 §4.2 defines the legacy comma-separated form that
/// real stylesheets still ship in bulk, including a bare `.5` alpha. The
/// computed value of both is the equivalent sRGB color (CSS Color 4 §15.1), so
/// the conversion happens here instead of adding a second color space to the
/// paint contract.
fn parse_hsl_color<'i>(
    input: &mut Parser<'i, '_>,
    preserve_space: bool,
) -> CssResult<'i, CssColor> {
    if let Some(origin) = parse_relative_origin(input)? {
        return parse_relative_color(
            input,
            crate::color::RelativeFunction::Hsl,
            origin,
            preserve_space,
        );
    }
    let location = input.current_source_location();
    let hue = parse_hue_or_none(input)?;
    let comma_syntax = input.try_parse(Parser::expect_comma).is_ok();
    if comma_syntax && hue.is_none() {
        return Err(location.new_custom_error(()));
    }
    let saturation = parse_hsl_percentage(input, comma_syntax)?;
    if comma_syntax {
        input.expect_comma()?;
    }
    let lightness = parse_hsl_percentage(input, comma_syntax)?;
    let alpha = if comma_syntax {
        if input.try_parse(Parser::expect_comma).is_ok() {
            parse_alpha_component(input, false)?
        } else {
            Some(1.0)
        }
    } else if input
        .try_parse(|candidate| candidate.expect_delim('/'))
        .is_ok()
    {
        parse_alpha_component(input, true)?
    } else {
        Some(1.0)
    };
    if !input.is_exhausted() {
        return Err(location.new_custom_error(()));
    }
    if comma_syntax && (saturation.is_none() || lightness.is_none() || alpha.is_none()) {
        return Err(location.new_custom_error(()));
    }
    if preserve_space
        || hue.is_none()
        || saturation.is_none()
        || lightness.is_none()
        || alpha.is_none()
    {
        let components = [
            hue.map(|value| value.rem_euclid(360.0)),
            saturation.map(|value| value.clamp(0.0, 1.0)),
            lightness.map(|value| value.clamp(0.0, 1.0)),
        ];
        return Ok(CssColor::Space(Box::new(crate::color::SpaceColor::new(
            crate::color::ColorSpace::Hsl,
            components,
            alpha,
        ))));
    }
    let (red, green, blue) = hsl_to_srgb(
        hue.unwrap_or(0.0),
        saturation.unwrap_or(0.0),
        lightness.unwrap_or(0.0),
    );
    Ok(CssColor::Srgb {
        red,
        green,
        blue,
        alpha: alpha.unwrap_or(1.0).clamp(0.0, 1.0),
    })
}

/// CSS Values 4 §5.2: a hue is a `<number>` in degrees or an `<angle>`.
fn parse_hue<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    let degrees = match input.next()?.clone() {
        Token::Number { value, .. } => value,
        Token::Dimension { value, unit, .. } => match unit.to_ascii_lowercase().as_str() {
            "deg" => value,
            "grad" => value * 0.9,
            "rad" => value.to_degrees(),
            "turn" => value * 360.0,
            _ => return Err(location.new_custom_error(())),
        },
        _ => return Err(location.new_custom_error(())),
    };
    // Hue is an angle and wraps, but an overflow token is still invalid.
    if degrees.is_finite() {
        Ok(degrees)
    } else {
        Err(location.new_custom_error(()))
    }
}

/// Saturation and lightness are `<percentage>`s in CSS Color 4. The legacy
/// CSS Color 3 grammar also accepted bare numbers in the 0-100 range, which
/// minified stylesheets still emit. The result is a `0.0..=1.0` fraction, or
/// `None` for the modern missing-component keyword (§4.4).
fn parse_hsl_percentage<'i>(
    input: &mut Parser<'i, '_>,
    comma_syntax: bool,
) -> CssResult<'i, Option<f32>> {
    let location = input.current_source_location();
    let value = match input.next()?.clone() {
        // `unit_value` is already normalized to `0.0..=1.0`.
        Token::Percentage { unit_value, .. } => Some(unit_value),
        Token::Number { value, .. } if comma_syntax => Some(value / 100.0),
        Token::Ident(value) if !comma_syntax && value.eq_ignore_ascii_case("none") => None,
        _ => return Err(location.new_custom_error(())),
    };
    if value.is_none_or(f32::is_finite) {
        Ok(value)
    } else {
        Err(location.new_custom_error(()))
    }
}

/// CSS Color 4 §12.2 hue-to-rgb conversion, with saturation and lightness as
/// fractions in `0.0..=1.0`.
fn hsl_to_srgb(hue_degrees: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    // Section 12.2 works in sixths of a turn, offset by the green sector.
    let turn = hue_degrees.rem_euclid(360.0) / 60.0;
    let second = chroma * (1.0 - ((turn % 2.0) - 1.0).abs());
    let (red, green, blue) = if turn < 1.0 {
        (chroma, second, 0.0)
    } else if turn < 2.0 {
        (second, chroma, 0.0)
    } else if turn < 3.0 {
        (0.0, chroma, second)
    } else if turn < 4.0 {
        (0.0, second, chroma)
    } else if turn < 5.0 {
        (second, 0.0, chroma)
    } else {
        (chroma, 0.0, second)
    };
    let match_value = lightness - chroma / 2.0;
    (
        rounded_rgb_channel((red + match_value) * 255.0),
        rounded_rgb_channel((green + match_value) * 255.0),
        rounded_rgb_channel((blue + match_value) * 255.0),
    )
}

fn parse_display<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Display> {
    let location = input.current_source_location();
    let mut words = Vec::new();
    while !input.is_exhausted() {
        words.push(input.expect_ident_cloned()?.to_ascii_lowercase());
        if words.len() > 3 {
            return Err(location.new_custom_error(()));
        }
    }
    if words.len() == 1
        && let Some(value) = parse_single_display(&words[0])
    {
        return Ok(value);
    }

    let mut outside = None;
    let mut inside = None;
    let mut list_item = false;
    for word in words {
        match word.as_str() {
            "block" if outside.is_none() => outside = Some(DisplayOutside::Block),
            "inline" if outside.is_none() => outside = Some(DisplayOutside::Inline),
            "run-in" if outside.is_none() => outside = Some(DisplayOutside::RunIn),
            "flow" if inside.is_none() => inside = Some(DisplayInside::Flow),
            "flow-root" if inside.is_none() => inside = Some(DisplayInside::FlowRoot),
            "table" if inside.is_none() => inside = Some(DisplayInside::Table),
            "flex" if inside.is_none() => inside = Some(DisplayInside::Flex),
            "grid" if inside.is_none() => inside = Some(DisplayInside::Grid),
            "ruby" if inside.is_none() => inside = Some(DisplayInside::Ruby),
            "list-item" if !list_item => list_item = true,
            _ => return Err(location.new_custom_error(())),
        }
    }
    let outside = outside.unwrap_or(DisplayOutside::Block);
    let inside = inside.unwrap_or(DisplayInside::Flow);
    if list_item && !matches!(inside, DisplayInside::Flow | DisplayInside::FlowRoot) {
        return Err(location.new_custom_error(()));
    }
    Ok(Display::Normal {
        outside,
        inside,
        list_item,
    })
}

fn parse_single_display(value: &str) -> Option<Display> {
    let normal = |outside, inside, list_item| Display::Normal {
        outside,
        inside,
        list_item,
    };
    Some(match value {
        "none" => Display::Box(DisplayBox::None),
        "contents" => Display::Box(DisplayBox::Contents),
        "block" | "flow" => normal(DisplayOutside::Block, DisplayInside::Flow, false),
        "inline" => normal(DisplayOutside::Inline, DisplayInside::Flow, false),
        "run-in" => normal(DisplayOutside::RunIn, DisplayInside::Flow, false),
        "flow-root" => normal(DisplayOutside::Block, DisplayInside::FlowRoot, false),
        "table" => normal(DisplayOutside::Block, DisplayInside::Table, false),
        "flex" | "-webkit-box" | "-webkit-flex" | "-ms-flexbox" => {
            normal(DisplayOutside::Block, DisplayInside::Flex, false)
        }
        "grid" => normal(DisplayOutside::Block, DisplayInside::Grid, false),
        "ruby" => normal(DisplayOutside::Inline, DisplayInside::Ruby, false),
        "list-item" => normal(DisplayOutside::Block, DisplayInside::Flow, true),
        "inline-block" => normal(DisplayOutside::Inline, DisplayInside::FlowRoot, false),
        "inline-table" => normal(DisplayOutside::Inline, DisplayInside::Table, false),
        "inline-flex" => normal(DisplayOutside::Inline, DisplayInside::Flex, false),
        "inline-grid" => normal(DisplayOutside::Inline, DisplayInside::Grid, false),
        "table-row-group" => Display::Internal(DisplayInternal::TableRowGroup),
        "table-header-group" => Display::Internal(DisplayInternal::TableHeaderGroup),
        "table-footer-group" => Display::Internal(DisplayInternal::TableFooterGroup),
        "table-row" => Display::Internal(DisplayInternal::TableRow),
        "table-cell" => Display::Internal(DisplayInternal::TableCell),
        "table-column-group" => Display::Internal(DisplayInternal::TableColumnGroup),
        "table-column" => Display::Internal(DisplayInternal::TableColumn),
        "table-caption" => Display::Internal(DisplayInternal::TableCaption),
        "ruby-base" => Display::Internal(DisplayInternal::RubyBase),
        "ruby-text" => Display::Internal(DisplayInternal::RubyText),
        "ruby-base-container" => Display::Internal(DisplayInternal::RubyBaseContainer),
        "ruby-text-container" => Display::Internal(DisplayInternal::RubyTextContainer),
        _ => return None,
    })
}

fn parse_size<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Size> {
    if let Ok(ident) = input.try_parse(Parser::expect_ident_cloned) {
        return match ident.to_ascii_lowercase().as_str() {
            "auto" => Ok(Size::Auto),
            "min-content" => Ok(Size::MinContent),
            "max-content" => Ok(Size::MaxContent),
            "stretch" => Ok(Size::Stretch),
            "fit-content" => Ok(Size::FitContent(None)),
            _ => Err(input.new_custom_error(())),
        };
    }
    if input
        .try_parse(|candidate| candidate.expect_function_matching("fit-content"))
        .is_ok()
    {
        let value = input.parse_nested_block(|nested| parse_length_percentage(nested, true))?;
        return Ok(Size::FitContent(Some(value)));
    }
    parse_length_percentage(input, true).map(Size::LengthPercentage)
}

fn parse_max_size<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, MaxSize> {
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("none"))
        .is_ok()
    {
        Ok(MaxSize::None)
    } else {
        parse_size(input).map(MaxSize::Size)
    }
}

fn parse_auto_length_percentage<'i>(
    input: &mut Parser<'i, '_>,
    non_negative: bool,
) -> CssResult<'i, AutoLengthPercentage> {
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("auto"))
        .is_ok()
    {
        Ok(AutoLengthPercentage::Auto)
    } else {
        parse_length_percentage(input, non_negative).map(AutoLengthPercentage::LengthPercentage)
    }
}

fn parse_border_width<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, BorderWidth> {
    if let Ok(ident) = input.try_parse(Parser::expect_ident_cloned) {
        return match ident.to_ascii_lowercase().as_str() {
            "thin" => Ok(BorderWidth::Thin),
            "medium" => Ok(BorderWidth::Medium),
            "thick" => Ok(BorderWidth::Thick),
            _ => Err(input.new_custom_error(())),
        };
    }
    let location = input.current_source_location();
    let value = parse_length_percentage(input, true)?;
    if value.is_length_only() {
        Ok(BorderWidth::Length(value))
    } else {
        Err(location.new_custom_error(()))
    }
}

/// CSS 2.1 §17.6.1: `border-spacing: <length>{1,2}`, and the spacing must not
/// be negative.
///
/// The grammar also accepts a `<percentage>`, which CSS 2.1 does not list.
/// HTML's `cellspacing` presentational hint feeds a percentage straight into
/// this property (render-core's user-agent declaration builder), and the table
/// solver already resolves a percentage against the table's used width, so
/// rejecting it here would drop a declaration the engine can honour.
fn parse_border_spacing<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, BorderSpacing> {
    let horizontal = parse_length_percentage(input, true)?;
    // The one-value form sets both components.
    let vertical = input
        .try_parse(parse_length_percentage_non_negative)
        .unwrap_or_else(|_| horizontal.clone());
    Ok(BorderSpacing {
        horizontal,
        vertical,
    })
}

/// Text Decoration 4 §2.1: `none`, or one or more line keywords joined by the
/// `||` combinator. Duplicates are rejected, because `||` requires each
/// component to appear at most once and `underline underline` is not a legal
/// value.
fn parse_text_decoration_line<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, TextDecorationLine> {
    let location = input.current_source_location();
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(TextDecorationLine::None);
    }
    let mut lines: Vec<TextDecorationLineKeyword> = Vec::new();
    while let Ok(ident) = input.try_parse(Parser::expect_ident_cloned) {
        let Some(keyword) = TextDecorationLineKeyword::parse(&ident) else {
            return Err(location.new_custom_error(()));
        };
        if lines.contains(&keyword) {
            return Err(location.new_custom_error(()));
        }
        lines.push(keyword);
    }
    // The `||` production needs at least one component, so a bare
    // `text-decoration-line:` is invalid rather than an empty set.
    if lines.is_empty() {
        return Err(location.new_custom_error(()));
    }
    Ok(TextDecorationLine::Lines(lines))
}

/// Text Decoration 4 §2.4: `auto | from-font | <length-percentage> |
/// <line-width>`, the last being `thin | medium | thick | <length [0,inf]>`.
fn parse_text_decoration_thickness<'i>(
    input: &mut Parser<'i, '_>,
) -> CssResult<'i, TextDecorationThickness> {
    if let Ok(ident) = input.try_parse(Parser::expect_ident_cloned) {
        return match ident.to_ascii_lowercase().as_str() {
            "auto" => Ok(TextDecorationThickness::Auto),
            "from-font" => Ok(TextDecorationThickness::FromFont),
            "thin" => Ok(TextDecorationThickness::Thin),
            "medium" => Ok(TextDecorationThickness::Medium),
            "thick" => Ok(TextDecorationThickness::Thick),
            _ => Err(input.new_custom_error(())),
        };
    }
    parse_length_percentage(input, true).map(TextDecorationThickness::Length)
}

/// CSS 2.1 §17.5.3: the `vertical-align` keywords plus the `<length>` /
/// `<percentage>` offset form. A `calc()` that cannot yet resolve keeps its
/// tree, so `vertical-align: calc(1em + 2px)` is not a syntax error.
fn parse_vertical_align<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, VerticalAlign> {
    if let Ok(ident) = input.try_parse(Parser::expect_ident_cloned) {
        return match ident.to_ascii_lowercase().as_str() {
            "baseline" => Ok(VerticalAlign::Baseline),
            "sub" => Ok(VerticalAlign::Sub),
            "super" => Ok(VerticalAlign::Super),
            "text-top" => Ok(VerticalAlign::TextTop),
            "text-bottom" => Ok(VerticalAlign::TextBottom),
            "middle" => Ok(VerticalAlign::Middle),
            "top" => Ok(VerticalAlign::Top),
            "bottom" => Ok(VerticalAlign::Bottom),
            _ => Err(input.new_custom_error(())),
        };
    }
    parse_length_percentage(input, false).map(VerticalAlign::Offset)
}

/// Absolute pixel size of the CSS absolute-size font keywords for a default
/// 16px `medium` user font. Shared by the computed-value and layout stages so
/// both agree on the mapping (CSS Fonts §4.1.3). The relative keywords
/// `larger` and `smaller` step one table entry away from the parent size.
#[must_use]
pub fn absolute_font_size_keyword(keyword: &str, parent_font_size: f32) -> Option<f32> {
    match keyword {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "larger" => Some(parent_font_size * 1.2),
        "smaller" => Some(parent_font_size / 1.2),
        _ => None,
    }
}

/// Resolve one `font-size` computed value to absolute CSS pixels.
///
/// CSS 2.1 §6.1.1 requires the computed value of `font-size` to be an
/// absolute length: `em` and `%` resolve against the inherited (parent) font
/// size, `rem` against the root font size. Returns `None` for values outside
/// the supported grammar; callers keep their previous interpretation then.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // CSS pixels are stored as f32 throughout layout
pub(crate) fn computed_font_size_px(
    value: &str,
    parent_font_size: f32,
    root_font_size: f32,
) -> Option<f32> {
    let lowered = value.trim().to_ascii_lowercase();
    if let Some(pixels) = absolute_font_size_keyword(&lowered, parent_font_size) {
        return Some(pixels);
    }
    crate::length::resolve_length_expr(
        &lowered,
        &crate::length::LengthContext {
            percentage_base: Some(f64::from(parent_font_size)),
            em_base: f64::from(parent_font_size),
            rem_base: f64::from(root_font_size),
            ..crate::length::LengthContext::default()
        },
    )
    .ok()
    .map(|pixels| pixels as f32)
    .filter(|pixels| pixels.is_finite() && *pixels > 0.0)
}

fn parse_length_percentage<'i>(
    input: &mut Parser<'i, '_>,
    non_negative: bool,
) -> CssResult<'i, LengthPercentage> {
    let location = input.current_source_location();
    let parsed = parse_top_numeric(input)?;
    let value = match parsed.node {
        CalcNode::Value(CalcValue::Number(0.0)) => LengthPercentage::Zero,
        CalcNode::Value(CalcValue::Length(value)) => LengthPercentage::Length(value),
        CalcNode::Value(CalcValue::Percentage(value)) => LengthPercentage::Percentage(value),
        expression if parsed.value_type.is_length_percentage() => {
            LengthPercentage::Calculation(Calculation {
                function: parsed
                    .function
                    .ok_or_else(|| location.new_custom_error(()))?,
                value_type: parsed.value_type,
                expression,
            })
        }
        _ => return Err(location.new_custom_error(())),
    };
    if non_negative && value.definitely_negative() {
        Err(location.new_custom_error(()))
    } else {
        Ok(value)
    }
}

fn parse_opacity<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    let parsed = parse_top_numeric(input)?;
    let value = match parsed.value_type {
        NumericType::Number | NumericType::Percentage => evaluate_scalar(&parsed.node),
        NumericType::Length | NumericType::LengthPercentage => None,
    }
    .ok_or_else(|| location.new_custom_error(()))?;
    Ok(value.clamp(0.0, 1.0))
}

fn parse_transform_list<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, TransformList> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(TransformList(Vec::new()));
    }
    let mut functions = Vec::new();
    let mut saw_function = false;
    while !input.is_exhausted() {
        if let Some(function) = parse_transform_function(input)? {
            functions.push(function);
        }
        saw_function = true;
    }
    if !saw_function {
        // An empty value is invalid; only the `none` keyword denotes the
        // empty list.
        return Err(input.new_custom_error(()));
    }
    Ok(TransformList(functions))
}

/// Parses one transform function. Returns `Ok(None)` for functions accepted
/// by the grammar that contribute an identity to the 2D pipeline
/// (`translateZ`, `scaleZ`).
fn parse_transform_function<'i>(
    input: &mut Parser<'i, '_>,
) -> CssResult<'i, Option<TransformFunction>> {
    let name = input.expect_function()?.to_string();
    input.parse_nested_block(|arguments| {
        let function = parse_transform_arguments(&name, arguments)?;
        if !arguments.is_exhausted() {
            return Err(arguments.current_source_location().new_custom_error(()));
        }
        Ok(function)
    })
}

fn parse_transform_arguments<'i>(
    name: &str,
    input: &mut Parser<'i, '_>,
) -> CssResult<'i, Option<TransformFunction>> {
    // Transform function names are ASCII case-insensitive per CSS syntax.
    let location = input.current_source_location();
    match name.to_ascii_lowercase().as_str() {
        "matrix" => {
            let values = parse_comma_separated_numbers(input, 6)?;
            Ok(Some(TransformFunction::Matrix([
                values[0], values[1], values[2], values[3], values[4], values[5],
            ])))
        }
        "matrix3d" => {
            let values = parse_comma_separated_numbers(input, 16)?;
            // A 4x4 matrix projects onto the 2D affine pipeline only when the
            // depth row and column vanish (m13/m23/m31/m32/m43 == 0 with
            // m33 == 1), the perspective row vanishes (m14/m24/m34 == 0 with
            // m44 == 1), and only the 2D translation entries m41/m42 survive.
            let affine = values[2] == 0.0
                && values[3] == 0.0
                && values[6] == 0.0
                && values[7] == 0.0
                && values[8] == 0.0
                && values[9] == 0.0
                && values[10] == 1.0
                && values[11] == 0.0
                && values[14] == 0.0
                && values[15] == 1.0;
            if !affine {
                return Err(location.new_custom_error(()));
            }
            Ok(Some(TransformFunction::Matrix([
                values[0], values[1], values[4], values[5], values[12], values[13],
            ])))
        }
        "translate" => {
            let x = parse_length_percentage(input, false)?;
            let y = input
                .try_parse(|input| {
                    input.expect_comma()?;
                    parse_length_percentage(input, false)
                })
                .unwrap_or(LengthPercentage::Zero);
            Ok(Some(TransformFunction::Translate(x, y)))
        }
        "translatex" => Ok(Some(TransformFunction::Translate(
            parse_length_percentage(input, false)?,
            LengthPercentage::Zero,
        ))),
        "translatey" => Ok(Some(TransformFunction::Translate(
            LengthPercentage::Zero,
            parse_length_percentage(input, false)?,
        ))),
        "translate3d" => {
            let x = parse_length_percentage(input, false)?;
            input.expect_comma()?;
            let y = parse_length_percentage(input, false)?;
            input.expect_comma()?;
            // The z offset is accepted by the grammar but cannot affect a 2D
            // pipeline; it is parsed and dropped.
            parse_length_percentage(input, false)?;
            Ok(Some(TransformFunction::Translate(x, y)))
        }
        "translatez" => {
            parse_length_percentage(input, false)?;
            Ok(None)
        }
        "scale" => {
            let x = parse_finite_number(input)?;
            let y = input
                .try_parse(|input| {
                    input.expect_comma()?;
                    parse_finite_number(input)
                })
                .unwrap_or(x);
            Ok(Some(TransformFunction::Scale(x, y)))
        }
        "scalex" => Ok(Some(TransformFunction::Scale(
            parse_finite_number(input)?,
            1.0,
        ))),
        "scaley" => Ok(Some(TransformFunction::Scale(
            1.0,
            parse_finite_number(input)?,
        ))),
        "scale3d" => {
            let x = parse_finite_number(input)?;
            input.expect_comma()?;
            let y = parse_finite_number(input)?;
            input.expect_comma()?;
            parse_finite_number(input)?;
            Ok(Some(TransformFunction::Scale(x, y)))
        }
        "scalez" => {
            parse_finite_number(input)?;
            Ok(None)
        }
        "rotate" | "rotatez" => Ok(Some(TransformFunction::Rotate(parse_angle_argument(
            input,
        )?))),
        "rotate3d" => {
            let x = parse_finite_number(input)?;
            input.expect_comma()?;
            let y = parse_finite_number(input)?;
            input.expect_comma()?;
            let z = parse_finite_number(input)?;
            input.expect_comma()?;
            let angle = parse_angle_argument(input)?;
            // Only a rotation about the z axis projects onto the 2D pipeline;
            // a negative axis component flips the rotation direction.
            if x == 0.0 && y == 0.0 && z != 0.0 {
                let direction = if z > 0.0 { angle } else { -angle };
                Ok(Some(TransformFunction::Rotate(direction)))
            } else {
                Err(location.new_custom_error(()))
            }
        }
        "skew" => {
            let ax = parse_angle_argument(input)?;
            let ay = input
                .try_parse(|input| {
                    input.expect_comma()?;
                    parse_angle_argument(input)
                })
                .unwrap_or(0.0);
            Ok(Some(TransformFunction::Skew(ax, ay)))
        }
        "skewx" => Ok(Some(TransformFunction::Skew(
            parse_angle_argument(input)?,
            0.0,
        ))),
        "skewy" => Ok(Some(TransformFunction::Skew(
            0.0,
            parse_angle_argument(input)?,
        ))),
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_comma_separated_numbers<'i>(
    input: &mut Parser<'i, '_>,
    count: usize,
) -> CssResult<'i, Vec<f32>> {
    let location = input.current_source_location();
    let values = input.parse_comma_separated(|input| {
        let value = input.expect_number().map_err(ParseError::from)?;
        if value.is_finite() {
            Ok(value)
        } else {
            Err(location.new_custom_error(()))
        }
    })?;
    if values.len() != count {
        return Err(location.new_custom_error(()));
    }
    Ok(values)
}

fn parse_angle_argument<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    match input.next()? {
        Token::Number { value, .. } if *value == 0.0 => Ok(0.0),
        Token::Dimension { value, unit, .. } => {
            // Angle units are ASCII case-insensitive per CSS syntax.
            let radians = match unit.to_ascii_lowercase().as_str() {
                "deg" => value.to_radians(),
                "grad" => *value * std::f32::consts::PI / 200.0,
                "rad" => *value,
                "turn" => *value * std::f32::consts::TAU,
                _ => return Err(location.new_custom_error(())),
            };
            // Overflow tokens must not leak non-finite angles into transform
            // math, mirroring the number-argument guard.
            if radians.is_finite() {
                Ok(radians)
            } else {
                Err(location.new_custom_error(()))
            }
        }
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_transform_origin<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, TransformOrigin> {
    // Slot-based assignment (CSS Transforms §4): `top`/`bottom` are
    // vertical-only, `left`/`right` horizontal-only, `center` fits whichever
    // slot is open, and bare lengths/percentages fill x then y. A trailing
    // length is the z offset, accepted but ignored by the 2D pipeline.
    let mut x: Option<LengthPercentage> = None;
    let mut y: Option<LengthPercentage> = None;
    let mut values = 0;
    while values < 3 && !input.is_exhausted() {
        let location = input.current_source_location();
        let keyword = input.try_parse(|input| {
            input
                .expect_ident()
                .map(|keyword| keyword.to_ascii_lowercase())
        });
        if let Ok(keyword) = keyword {
            let percentage =
                transform_origin_keyword(&keyword).ok_or_else(|| location.new_custom_error(()))?;
            let resolved = LengthPercentage::Percentage(percentage);
            match (values, keyword.as_str()) {
                // First value: a vertical-only keyword fills y directly.
                (0, "top" | "bottom") => y = Some(resolved),
                (0, _) => x = Some(resolved),
                // Second value fills the only slot still open and must belong
                // to that slot's axis.
                (1, "top" | "bottom") if x.is_none() => {
                    return Err(location.new_custom_error(()));
                }
                (1, _) if x.is_none() => x = Some(resolved),
                (1, "left" | "right") => return Err(location.new_custom_error(())),
                (1, _) => y = Some(resolved),
                _ => return Err(location.new_custom_error(())),
            }
        } else {
            let value = parse_length_percentage(input, false)?;
            match values {
                0 => x = Some(value),
                // A length after a leading vertical keyword fills x.
                1 if x.is_none() => x = Some(value),
                1 => y = Some(value),
                // Third value: the z position, a plain length, ignored in 2D.
                2 if value.is_length_only() => {}
                _ => return Err(location.new_custom_error(())),
            }
        }
        values += 1;
    }
    if values == 0 {
        // An empty value is invalid; the 50% 50% default belongs to the
        // initial-value path, not the parser.
        return Err(input.new_custom_error(()));
    }
    Ok(TransformOrigin(
        x.unwrap_or(LengthPercentage::Percentage(0.5)),
        y.unwrap_or(LengthPercentage::Percentage(0.5)),
    ))
}

fn transform_origin_keyword(keyword: &str) -> Option<f32> {
    match keyword {
        "left" | "top" => Some(0.0),
        "center" => Some(0.5),
        "right" | "bottom" => Some(1.0),
        _ => None,
    }
}

fn parse_length_percentage_non_negative<'i>(
    input: &mut Parser<'i, '_>,
) -> CssResult<'i, LengthPercentage> {
    parse_length_percentage(input, true)
}

fn parse_non_negative_number<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    match input.next()?.clone() {
        Token::Number { value, .. } if value.is_finite() && value >= 0.0 => Ok(value),
        _ => Err(location.new_custom_error(())),
    }
}

/// Parse a `<number>` argument, rejecting the non-finite values that numeric
/// overflow tokens can carry so transform math stays bounded.
fn parse_finite_number<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    let value = input.expect_number()?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(location.new_custom_error(()))
    }
}

fn parse_integer<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, i32> {
    let location = input.current_source_location();
    match input.next()?.clone() {
        Token::Number {
            int_value: Some(value),
            ..
        } => Ok(value),
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_flex_basis<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, FlexBasis> {
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("auto"))
        .is_ok()
    {
        Ok(FlexBasis::Auto)
    } else if input
        .try_parse(|candidate| candidate.expect_ident_matching("content"))
        .is_ok()
    {
        Ok(FlexBasis::Content)
    } else {
        parse_length_percentage(input, true).map(FlexBasis::LengthPercentage)
    }
}

/// `column-count` (CSS Multi-column Layout §3.1): `auto`, or a positive integer.
fn parse_column_count<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, ColumnCount> {
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("auto"))
        .is_ok()
    {
        return Ok(ColumnCount::Auto);
    }
    let location = input.current_source_location();
    let count = parse_integer(input)?;
    u32::try_from(count)
        .ok()
        .filter(|count| *count >= 1)
        .map(ColumnCount::Count)
        .ok_or_else(|| location.new_custom_error(()))
}

/// `column-width` (CSS Multi-column Layout §3.2): `auto`, or a non-negative
/// length. A percentage is not a valid column width.
fn parse_column_width<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, ColumnWidth> {
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("auto"))
        .is_ok()
    {
        return Ok(ColumnWidth::Auto);
    }
    let location = input.current_source_location();
    match parse_length_percentage(input, true)? {
        value @ (LengthPercentage::Length(_) | LengthPercentage::Zero) => {
            Ok(ColumnWidth::Length(value))
        }
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_gap<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Gap> {
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("normal"))
        .is_ok()
    {
        Ok(Gap::Normal)
    } else {
        parse_length_percentage(input, true).map(Gap::LengthPercentage)
    }
}

/// Parse one `<grid-line>` (CSS Grid §8.2) with the forms real sheets use:
/// `auto`, a nonzero `<integer>` (negative counts from the explicit grid
/// end), `span`, `span <integer>`, `<integer> span`, and `span` + integer in
/// either order. Line-name custom idents are rejected until named tracks are
/// supported by the layout engine.
fn parse_grid_line<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, GridLine> {
    let location = input.current_source_location();
    let integer = |input: &mut Parser<'i, '_>| -> CssResult<'i, i32> {
        let value = parse_integer(input)?;
        // CSS Grid §8.1: a line number of zero makes the declaration invalid.
        if value == 0 {
            return Err(input.new_custom_error(()));
        }
        Ok(value)
    };
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("auto"))
        .is_ok()
    {
        return Ok(GridLine::Auto);
    }
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("span"))
        .is_ok()
    {
        let count = input.try_parse(integer).unwrap_or(1);
        if count < 1 {
            return Err(location.new_custom_error(()));
        }
        return Ok(GridLine::Span(count));
    }
    let value = integer(input)?;
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("span"))
        .is_ok()
    {
        if value < 1 {
            return Err(location.new_custom_error(()));
        }
        return Ok(GridLine::Span(value));
    }
    Ok(GridLine::Line(value))
}

/// Expand a `grid-column` / `grid-row` shorthand into its start/end longhand
/// values (CSS Grid §8.3): `a / b` sets both sides, a single component sets
/// the start side and leaves the end side `auto`. Returns `None` when the
/// value is not a valid axis placement, so the caller keeps the declaration
/// for diagnostics instead of dropping it silently.
pub(crate) fn expand_grid_axis_shorthand(css: &str) -> Option<(String, String)> {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    parser
        .parse_entirely(|input| {
            let start = parse_grid_line(input)?;
            let end = if input
                .try_parse(|candidate| candidate.expect_delim('/'))
                .is_ok()
            {
                // Two spans on one axis cannot resolve against a grid
                // (CSS Grid §8.1); the shorthand is invalid as a whole.
                if matches!(start, GridLine::Span(_))
                    && input
                        .try_parse(|candidate| candidate.expect_ident_matching("span"))
                        .is_ok()
                {
                    return Err(input.new_custom_error(()));
                }
                parse_grid_line(input)?
            } else {
                GridLine::Auto
            };
            Ok((start.to_css(), end.to_css()))
        })
        .ok()
}

const MAX_PARSED_GRID_TRACKS: usize = 4_096;

enum ParsedGridComponent {
    Tracks(Vec<GridTrack>),
    AutoRepeat {
        kind: GridAutoRepeat,
        tracks: Vec<GridTrack>,
    },
}

fn parse_grid_template<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, GridTemplate> {
    if input
        .try_parse(|candidate| candidate.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(GridTemplate::None);
    }

    let location = input.current_source_location();
    let mut tracks = Vec::new();
    let mut auto_repeat = None;
    while !input.is_exhausted() {
        match parse_grid_component(input)? {
            ParsedGridComponent::Tracks(component_tracks) => {
                if auto_repeat.is_some()
                    || tracks.len().saturating_add(component_tracks.len()) > MAX_PARSED_GRID_TRACKS
                {
                    return Err(location.new_custom_error(()));
                }
                tracks.extend(component_tracks);
            }
            ParsedGridComponent::AutoRepeat {
                kind,
                tracks: repeated,
            } => {
                // This slice supports a complete standalone auto-repeat. CSS
                // permits it alongside fixed tracks, whose repetition and
                // empty-track collapse require line-name-aware placement.
                if auto_repeat.is_some() || !tracks.is_empty() || repeated.is_empty() {
                    return Err(location.new_custom_error(()));
                }
                auto_repeat = Some((kind, repeated));
            }
        }
    }

    if let Some((kind, tracks)) = auto_repeat {
        Ok(GridTemplate::AutoRepeat { kind, tracks })
    } else if tracks.is_empty() {
        Err(location.new_custom_error(()))
    } else {
        Ok(GridTemplate::Tracks(tracks))
    }
}

fn parse_grid_component<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, ParsedGridComponent> {
    if input
        .try_parse(|candidate| candidate.expect_function_matching("repeat"))
        .is_ok()
    {
        return input.parse_nested_block(|nested| {
            if let Ok(kind) = nested.try_parse(parse_grid_auto_repeat_keyword) {
                nested.expect_comma()?;
                let tracks = parse_grid_track_sequence(nested)?;
                if !tracks.iter().all(grid_track_is_fixed_repetition_size) {
                    return Err(nested.new_custom_error(()));
                }
                return Ok(ParsedGridComponent::AutoRepeat { kind, tracks });
            }

            let location = nested.current_source_location();
            let count = match nested.next()?.clone() {
                Token::Number {
                    int_value: Some(value),
                    ..
                } if value > 0 => usize::try_from(value)
                    .ok()
                    .filter(|value| *value <= MAX_PARSED_GRID_TRACKS)
                    .ok_or_else(|| location.new_custom_error(()))?,
                _ => return Err(location.new_custom_error(())),
            };
            nested.expect_comma()?;
            let repeated = parse_grid_track_sequence(nested)?;
            let expanded_len = repeated
                .len()
                .checked_mul(count)
                .filter(|length| *length <= MAX_PARSED_GRID_TRACKS)
                .ok_or_else(|| location.new_custom_error(()))?;
            let mut expanded = Vec::with_capacity(expanded_len);
            for _ in 0..count {
                expanded.extend(repeated.iter().cloned());
            }
            Ok(ParsedGridComponent::Tracks(expanded))
        });
    }

    parse_grid_track(input).map(|track| ParsedGridComponent::Tracks(vec![track]))
}

fn parse_grid_track_sequence<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, Vec<GridTrack>> {
    let location = input.current_source_location();
    let mut tracks = Vec::new();
    while !input.is_exhausted() {
        if tracks.len() >= MAX_PARSED_GRID_TRACKS {
            return Err(location.new_custom_error(()));
        }
        tracks.push(parse_grid_track(input)?);
    }
    if tracks.is_empty() {
        Err(location.new_custom_error(()))
    } else {
        Ok(tracks)
    }
}

fn parse_grid_track<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, GridTrack> {
    if input
        .try_parse(|candidate| candidate.expect_function_matching("minmax"))
        .is_ok()
    {
        return input.parse_nested_block(|nested| {
            let minimum = parse_length_percentage(nested, true)?;
            nested.expect_comma()?;
            let maximum = parse_grid_track_breadth(nested)?;
            Ok(GridTrack::MinMax { minimum, maximum })
        });
    }
    parse_grid_track_breadth(input).map(GridTrack::Breadth)
}

fn parse_grid_track_breadth<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, GridTrackBreadth> {
    if let Ok(fraction) = input.try_parse(parse_grid_fraction) {
        Ok(GridTrackBreadth::Fraction(fraction))
    } else {
        parse_length_percentage(input, true).map(GridTrackBreadth::LengthPercentage)
    }
}

fn parse_grid_fraction<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    match input.next()?.clone() {
        Token::Dimension { value, unit, .. }
            if value.is_finite() && value >= 0.0 && unit.eq_ignore_ascii_case("fr") =>
        {
            Ok(value)
        }
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_grid_auto_repeat_keyword<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, GridAutoRepeat> {
    let location = input.current_source_location();
    let keyword = input.expect_ident_cloned()?;
    match keyword.to_ascii_lowercase().as_str() {
        "auto-fill" => Ok(GridAutoRepeat::Fill),
        "auto-fit" => Ok(GridAutoRepeat::Fit),
        _ => Err(location.new_custom_error(())),
    }
}

fn grid_track_is_fixed_repetition_size(track: &GridTrack) -> bool {
    matches!(
        track,
        GridTrack::Breadth(GridTrackBreadth::LengthPercentage(_)) | GridTrack::MinMax { .. }
    )
}

pub(crate) fn expand_gap_shorthand(css: &str) -> Option<(String, String)> {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    parser
        .parse_entirely(|input| {
            let row = parse_gap(input)?;
            let column = input.try_parse(parse_gap).unwrap_or_else(|_| row.clone());
            Ok((row.to_css(), column.to_css()))
        })
        .ok()
}

/// `columns` (CSS Multi-column Layout §3): `<'column-width'> || <'column-count'>`,
/// where an omitted longhand is `auto`. Each token is classified by the grammar
/// it parses under, and a token that fits neither, or a repeated kind, is invalid.
/// Returns the `(column-width, column-count)` longhand values.
pub(crate) fn expand_columns_shorthand(css: &str) -> Option<(String, String)> {
    let mut width = None;
    let mut count = None;
    for token in css.split_ascii_whitespace() {
        if token.eq_ignore_ascii_case("auto") {
            continue;
        }
        let is_count = matches!(parse_typed_property("column-count", token), Some(Ok(_)));
        let is_width = matches!(parse_typed_property("column-width", token), Some(Ok(_)));
        let slot = match (is_count, is_width) {
            (true, _) => &mut count,
            (false, true) => &mut width,
            (false, false) => return None,
        };
        if slot.replace(token).is_some() {
            return None;
        }
    }
    Some((
        width.unwrap_or("auto").to_owned(),
        count.unwrap_or("auto").to_owned(),
    ))
}

pub(crate) fn expand_flex_shorthand(css: &str) -> Option<(String, String, String)> {
    if css.eq_ignore_ascii_case("none") {
        return Some(("0".to_owned(), "0".to_owned(), "auto".to_owned()));
    }
    if css.eq_ignore_ascii_case("auto") {
        return Some(("1".to_owned(), "1".to_owned(), "auto".to_owned()));
    }
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    parser
        .parse_entirely(|input| {
            if let Ok(grow) = input.try_parse(parse_non_negative_number) {
                let shrink = input.try_parse(parse_non_negative_number).unwrap_or(1.0);
                let basis =
                    input
                        .try_parse(parse_flex_basis)
                        .unwrap_or(FlexBasis::LengthPercentage(LengthPercentage::Percentage(
                            0.0,
                        )));
                Ok((format_number(grow), format_number(shrink), basis.to_css()))
            } else {
                let basis = parse_flex_basis(input)?;
                Ok(("1".to_owned(), "1".to_owned(), basis.to_css()))
            }
        })
        .ok()
}

struct ParsedNumeric {
    node: CalcNode,
    value_type: NumericType,
    function: Option<MathFunction>,
}

fn parse_top_numeric<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, ParsedNumeric> {
    let location = input.current_source_location();
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } if value.is_finite() => Ok(ParsedNumeric {
            node: CalcNode::Value(CalcValue::Number(value)),
            value_type: NumericType::Number,
            function: None,
        }),
        Token::Percentage { unit_value, .. } if unit_value.is_finite() => Ok(ParsedNumeric {
            node: CalcNode::Value(CalcValue::Percentage(unit_value)),
            value_type: NumericType::Percentage,
            function: None,
        }),
        Token::Dimension { value, unit, .. } if value.is_finite() => {
            let unit = LengthUnit::parse(&unit).ok_or_else(|| location.new_custom_error(()))?;
            Ok(ParsedNumeric {
                node: CalcNode::Value(CalcValue::Length(Length { value, unit })),
                value_type: NumericType::Length,
                function: None,
            })
        }
        Token::Function(name) => {
            let function =
                parse_math_function(&name).ok_or_else(|| location.new_custom_error(()))?;
            let typed = input.parse_nested_block(|nested| parse_function_body(nested, function))?;
            Ok(ParsedNumeric {
                node: typed.node,
                value_type: typed.value_type,
                function: Some(function),
            })
        }
        _ => Err(location.new_custom_error(())),
    }
}

struct TypedNode {
    node: CalcNode,
    value_type: NumericType,
}

fn parse_function_body<'i>(
    input: &mut Parser<'i, '_>,
    function: MathFunction,
) -> CssResult<'i, TypedNode> {
    match function {
        MathFunction::Calc => parse_sum(input),
        MathFunction::Min | MathFunction::Max => {
            let location = input.current_source_location();
            let values = input.parse_comma_separated(parse_sum)?;
            let value_type = common_type(&values).ok_or_else(|| location.new_custom_error(()))?;
            let nodes = values.into_iter().map(|value| value.node).collect();
            Ok(TypedNode {
                node: if function == MathFunction::Min {
                    CalcNode::Min(nodes)
                } else {
                    CalcNode::Max(nodes)
                },
                value_type,
            })
        }
        MathFunction::Clamp => {
            let location = input.current_source_location();
            let values = input.parse_comma_separated(parse_sum)?;
            if values.len() != 3 {
                return Err(location.new_custom_error(()));
            }
            let value_type = common_type(&values).ok_or_else(|| location.new_custom_error(()))?;
            let mut values = values.into_iter();
            let minimum = values.next().expect("clamp length checked").node;
            let preferred = values.next().expect("clamp length checked").node;
            let maximum = values.next().expect("clamp length checked").node;
            Ok(TypedNode {
                node: CalcNode::Clamp {
                    minimum: Box::new(minimum),
                    preferred: Box::new(preferred),
                    maximum: Box::new(maximum),
                },
                value_type,
            })
        }
    }
}

fn parse_sum<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, TypedNode> {
    let location = input.current_source_location();
    let first = parse_product(input)?;
    let mut value_type = first.value_type;
    let mut rest = Vec::new();
    loop {
        let operator = if input
            .try_parse(|candidate| candidate.expect_delim('+'))
            .is_ok()
        {
            SumOperator::Add
        } else if input
            .try_parse(|candidate| candidate.expect_delim('-'))
            .is_ok()
        {
            SumOperator::Subtract
        } else {
            break;
        };
        let right = parse_product(input)?;
        value_type = value_type
            .add(right.value_type)
            .ok_or_else(|| location.new_custom_error(()))?;
        rest.push((operator, right.node));
    }
    if rest.is_empty() {
        Ok(first)
    } else {
        Ok(TypedNode {
            node: CalcNode::Sum {
                first: Box::new(first.node),
                rest,
            },
            value_type,
        })
    }
}

fn parse_product<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, TypedNode> {
    let location = input.current_source_location();
    let first = parse_primary(input)?;
    let mut value_type = first.value_type;
    let mut rest = Vec::new();
    loop {
        let operator = if input
            .try_parse(|candidate| candidate.expect_delim('*'))
            .is_ok()
        {
            ProductOperator::Multiply
        } else if input
            .try_parse(|candidate| candidate.expect_delim('/'))
            .is_ok()
        {
            ProductOperator::Divide
        } else {
            break;
        };
        let right = parse_primary(input)?;
        value_type = match operator {
            ProductOperator::Multiply => match (value_type, right.value_type) {
                (NumericType::Number, other) | (other, NumericType::Number) => other,
                _ => return Err(location.new_custom_error(())),
            },
            ProductOperator::Divide if right.value_type == NumericType::Number => value_type,
            ProductOperator::Divide => return Err(location.new_custom_error(())),
        };
        if operator == ProductOperator::Divide && evaluate_scalar(&right.node) == Some(0.0) {
            return Err(location.new_custom_error(()));
        }
        rest.push((operator, right.node));
    }
    if rest.is_empty() {
        Ok(first)
    } else {
        Ok(TypedNode {
            node: CalcNode::Product {
                first: Box::new(first.node),
                rest,
            },
            value_type,
        })
    }
}

fn parse_primary<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, TypedNode> {
    let location = input.current_source_location();
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } if value.is_finite() => Ok(TypedNode {
            node: CalcNode::Value(CalcValue::Number(value)),
            value_type: NumericType::Number,
        }),
        Token::Percentage { unit_value, .. } if unit_value.is_finite() => Ok(TypedNode {
            node: CalcNode::Value(CalcValue::Percentage(unit_value)),
            value_type: NumericType::Percentage,
        }),
        Token::Dimension { value, unit, .. } if value.is_finite() => {
            let unit = LengthUnit::parse(&unit).ok_or_else(|| location.new_custom_error(()))?;
            Ok(TypedNode {
                node: CalcNode::Value(CalcValue::Length(Length { value, unit })),
                value_type: NumericType::Length,
            })
        }
        Token::ParenthesisBlock => input.parse_nested_block(|nested| {
            let value = parse_sum(nested)?;
            Ok(TypedNode {
                node: CalcNode::Parentheses(Box::new(value.node)),
                value_type: value.value_type,
            })
        }),
        Token::Function(name) => {
            let function =
                parse_math_function(&name).ok_or_else(|| location.new_custom_error(()))?;
            input.parse_nested_block(|nested| parse_function_body(nested, function))
        }
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_math_function(name: &str) -> Option<MathFunction> {
    if name.eq_ignore_ascii_case("calc") {
        Some(MathFunction::Calc)
    } else if name.eq_ignore_ascii_case("min") {
        Some(MathFunction::Min)
    } else if name.eq_ignore_ascii_case("max") {
        Some(MathFunction::Max)
    } else if name.eq_ignore_ascii_case("clamp") {
        Some(MathFunction::Clamp)
    } else {
        None
    }
}

fn common_type(values: &[TypedNode]) -> Option<NumericType> {
    let mut values = values.iter();
    let mut value_type = values.next()?.value_type;
    for value in values {
        value_type = value_type.add(value.value_type)?;
    }
    Some(value_type)
}

fn evaluate_scalar(node: &CalcNode) -> Option<f32> {
    match node {
        CalcNode::Value(CalcValue::Number(value) | CalcValue::Percentage(value)) => Some(*value),
        CalcNode::Value(CalcValue::Length(_)) => None,
        CalcNode::Parentheses(value) => evaluate_scalar(value),
        CalcNode::Sum { first, rest } => {
            let mut result = evaluate_scalar(first)?;
            for (operator, value) in rest {
                let value = evaluate_scalar(value)?;
                result = match operator {
                    SumOperator::Add => result + value,
                    SumOperator::Subtract => result - value,
                };
            }
            result.is_finite().then_some(result)
        }
        CalcNode::Product { first, rest } => {
            let mut result = evaluate_scalar(first)?;
            for (operator, value) in rest {
                let value = evaluate_scalar(value)?;
                result = match operator {
                    ProductOperator::Multiply => result * value,
                    ProductOperator::Divide if value != 0.0 => result / value,
                    ProductOperator::Divide => return None,
                };
            }
            result.is_finite().then_some(result)
        }
        CalcNode::Min(values) => values
            .iter()
            .map(evaluate_scalar)
            .try_fold(f32::INFINITY, |current, value| Some(current.min(value?))),
        CalcNode::Max(values) => values
            .iter()
            .map(evaluate_scalar)
            .try_fold(f32::NEG_INFINITY, |current, value| {
                Some(current.max(value?))
            }),
        CalcNode::Clamp {
            minimum,
            preferred,
            maximum,
        } => Some(
            evaluate_scalar(preferred)?
                .max(evaluate_scalar(minimum)?)
                .min(evaluate_scalar(maximum)?),
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{
        AspectRatio, Containment, CssColor, Display, DisplayInside, DisplayOutside, Length,
        LengthPercentage, LengthUnit, MaxSize, PropertyParseError, Size, TextAlign,
        TransformFunction, TransformList, TransformOrigin, TypedPropertyValue,
        parse_typed_property,
    };

    fn parse(name: &str, css: &str) -> TypedPropertyValue {
        parse_typed_property(name, css)
            .expect("supported property")
            .expect("valid value")
    }

    fn srgb(css: &str) -> (u8, u8, u8, f32) {
        match parse("color", css) {
            TypedPropertyValue::Color(color) => {
                let resolved = crate::color::resolve_absolute(&color)
                    .unwrap_or_else(|| panic!("expected an absolute color, got {color:?}"));
                let (red, green, blue) = resolved.to_srgb8();
                (red, green, blue, resolved.alpha.clamp(0.0, 1.0))
            }
            other => panic!("expected an srgb color, got {other:?}"),
        }
    }

    /// CSS Color 4 §4.3/§12.2 and the legacy CSS Color 3 §4.2 comma form.
    #[test]
    fn parses_hsl_colors_in_both_syntaxes() {
        assert_eq!(srgb("hsl(0, 0%, 100%)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("hsl(0 0% 100%)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("hsl(0, 0%, 0%)"), (0, 0, 0, 1.0));
        // Minified legacy alpha, the shape real sheets ship.
        assert_eq!(srgb("hsla(0,0%,100%,.5)"), (255, 255, 255, 0.5));
        assert_eq!(srgb("hsla(0, 0%, 0%, 0.25)"), (0, 0, 0, 0.25));
        assert_eq!(srgb("hsl(120 100% 50% / 50%)"), (0, 255, 0, 0.5));
        // Every hue sector, plus angle units and hue wraparound.
        assert_eq!(srgb("hsl(0, 100%, 50%)"), (255, 0, 0, 1.0));
        assert_eq!(srgb("hsl(60, 100%, 50%)"), (255, 255, 0, 1.0));
        assert_eq!(srgb("hsl(120, 100%, 50%)"), (0, 255, 0, 1.0));
        assert_eq!(srgb("hsl(180, 100%, 50%)"), (0, 255, 255, 1.0));
        assert_eq!(srgb("hsl(240, 100%, 50%)"), (0, 0, 255, 1.0));
        assert_eq!(srgb("hsl(300, 100%, 50%)"), (255, 0, 255, 1.0));
        assert_eq!(srgb("hsl(360, 100%, 50%)"), (255, 0, 0, 1.0));
        // Halfway inside a sector the second channel is interpolated.
        assert_eq!(srgb("hsl(30, 100%, 50%)"), (255, 128, 0, 1.0));
        assert_eq!(srgb("hsl(210, 100%, 50%)"), (0, 128, 255, 1.0));
        assert_eq!(srgb("hsl(480deg 100% 50%)"), (0, 255, 0, 1.0));
        assert_eq!(srgb("hsl(0.5turn 100% 50%)"), (0, 255, 255, 1.0));
        assert_eq!(srgb("hsl(200grad 100% 50%)"), (0, 255, 255, 1.0));
        assert_eq!(srgb("hsl(3.14159rad 100% 50%)"), (0, 255, 255, 1.0));
        // Out-of-range saturation/lightness clamp; hue is an angle and wraps.
        assert_eq!(srgb("hsl(-60, 100%, 50%)"), (255, 0, 255, 1.0));
        assert_eq!(srgb("hsl(0, 200%, 50%)"), (255, 0, 0, 1.0));
        assert_eq!(srgb("hsl(0, 0%, 150%)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("hsl(0, 0%, -50%)"), (0, 0, 0, 1.0));
        // Percentage alpha clamps, and every hue-relative color function is
        // out of scope, so it must stay an error rather than a wrong color.
        assert_eq!(srgb("hsl(0 0% 0% / 200%)"), (0, 0, 0, 1.0));
        // CSS Color 4 §4.3 keeps `hsla()` as a legacy alias, so a fourth
        // comma argument is accepted under the `hsl()` name too, as in every
        // shipping engine.
        assert_eq!(srgb("hsl(0, 0%, 0%, 0%)"), (0, 0, 0, 0.0));
        for invalid in [
            "hsl(0, 0%)",
            "hsl(0 0% 0% / )",
            "hsl(0deg, 0%)",
            "hsl(0, 0px, 0%)",
            "hsl(red, 0%, 0%)",
            "hsl(0 0% 0% / 0 0)",
            "hsl(0 0% 0% ) extra",
        ] {
            assert!(
                parse_typed_property("color", invalid)
                    .expect("supported property")
                    .is_err(),
                "{invalid} must not parse as a color"
            );
        }
    }

    /// CSS Backgrounds 3 §3.2 `<bg-image>#`.
    #[test]
    fn accepts_comma_separated_background_image_layers() {
        for (value, expected) in [
            ("url(a.svg)", "url(a.svg)"),
            ("url(a.svg),none", "url(a.svg),none"),
            ("none,url(a.svg)", "none,url(a.svg)"),
            (
                "url(a.svg),linear-gradient(red,blue)",
                "url(a.svg),linear-gradient(red,blue)",
            ),
            (
                "linear-gradient(red,blue),url(b.png),none",
                "linear-gradient(red,blue),url(b.png),none",
            ),
        ] {
            assert_eq!(
                parse("background-image", value),
                TypedPropertyValue::BackgroundImage(expected.to_owned()),
                "{value}"
            );
        }
        for invalid in ["url(a.svg),", ",url(a.svg)", "url(a.svg),,"] {
            assert!(
                parse_typed_property("background-image", invalid)
                    .expect("supported property")
                    .is_err(),
                "{invalid} must not parse"
            );
        }
    }

    fn transform(css: &str) -> TransformList {
        match parse("transform", css) {
            TypedPropertyValue::Transform(list) => list,
            other => panic!("expected a transform list, got {other:?}"),
        }
    }

    #[test]
    fn parses_aspect_ratio_forms_and_rejects_invalid_ratios() {
        assert_eq!(
            parse("aspect-ratio", "16 / 9"),
            TypedPropertyValue::AspectRatio(AspectRatio::Ratio(16.0 / 9.0))
        );
        assert_eq!(
            parse("aspect-ratio", "auto"),
            TypedPropertyValue::AspectRatio(AspectRatio::Auto)
        );
        assert_eq!(
            parse("aspect-ratio", "auto 4 / 3"),
            TypedPropertyValue::AspectRatio(AspectRatio::Ratio(4.0 / 3.0))
        );
        for value in ["0", "16 / 0", "-1 / 2", "16 / -9"] {
            assert!(
                parse_typed_property("aspect-ratio", value)
                    .expect("supported property")
                    .is_err()
            );
        }
    }

    /// CSS Containment 2 §2 grammar:
    /// `none | strict | content | [ [ size | inline-size ] || layout || style || paint ]`.
    #[test]
    fn parses_contain_keywords_and_longhand_combination() {
        let containment = |css: &str| match parse("contain", css) {
            TypedPropertyValue::Contain(value) => value,
            other => panic!("expected contain, got {other:?}"),
        };
        assert_eq!(containment("none"), Containment::default());
        assert_eq!(
            containment("strict"),
            Containment {
                size: true,
                layout: true,
                style: true,
                paint: true,
                ..Containment::default()
            }
        );
        assert_eq!(
            containment("content"),
            Containment {
                layout: true,
                style: true,
                paint: true,
                ..Containment::default()
            }
        );
        assert_eq!(
            containment("size"),
            Containment {
                size: true,
                ..Containment::default()
            }
        );
        assert_eq!(
            containment("inline-size"),
            Containment {
                inline_size: true,
                ..Containment::default()
            }
        );
        // `||` accepts any order.
        assert_eq!(containment("paint layout"), containment("layout paint"));
        assert_eq!(
            containment("paint layout"),
            Containment {
                layout: true,
                paint: true,
                ..Containment::default()
            }
        );
        assert_eq!(
            containment("style size layout"),
            Containment {
                size: true,
                layout: true,
                style: true,
                ..Containment::default()
            }
        );
        // Serialization follows grammar order rather than source order.
        assert_eq!(
            containment("paint style inline-size").to_css(),
            "inline-size style paint"
        );
        assert_eq!(containment("strict").to_css(), "size layout style paint");
        assert_eq!(containment("content").to_css(), "layout style paint");
        assert_eq!(containment("none").to_css(), "none");
    }

    #[test]
    fn rejects_invalid_contain_values() {
        for invalid in [
            "",
            "size layout paint size",
            "size inline-size",
            "inline-size size",
            "layout layout",
            "strict layout",
            "content paint",
            "none paint",
            "auto",
            "size, layout",
        ] {
            assert!(
                parse_typed_property("contain", invalid)
                    .expect("supported property")
                    .is_err(),
                "{invalid:?} must not parse as contain"
            );
        }
    }

    fn origin(css: &str) -> TransformOrigin {
        match parse("transform-origin", css) {
            TypedPropertyValue::TransformOrigin(origin) => origin,
            other => panic!("expected a transform origin, got {other:?}"),
        }
    }

    fn px(value: f32) -> LengthPercentage {
        LengthPercentage::Length(Length {
            value,
            unit: LengthUnit::Px,
        })
    }

    fn invalid(name: &str, css: &str) -> PropertyParseError {
        parse_typed_property(name, css)
            .expect("supported property")
            .expect_err("value must be rejected")
    }

    #[test]
    fn parses_modern_and_legacy_display_syntax_to_one_model() {
        let expected = TypedPropertyValue::Display(Display::Normal {
            outside: DisplayOutside::Inline,
            inside: DisplayInside::FlowRoot,
            list_item: false,
        });
        assert_eq!(parse("display", "inline-block"), expected);
        assert_eq!(parse("display", "inline flow-root"), expected);
        assert!(
            parse_typed_property("display", "inline flex list-item")
                .expect("supported")
                .is_err()
        );
    }

    #[test]
    fn preserves_mixed_length_percentage_math() {
        let value = parse("width", "calc(100% - 2rem)");
        assert_eq!(value.to_css(), "calc(100% - 2rem)");
        assert!(matches!(
            value,
            TypedPropertyValue::Size(Size::LengthPercentage(LengthPercentage::Calculation(_)))
        ));
    }

    #[test]
    fn rejects_unknown_units_dimensions_and_substitution_token_fusion() {
        assert!(
            parse_typed_property("width", "1furlong")
                .expect("supported")
                .is_err()
        );
        assert!(
            parse_typed_property("width", "calc(1px + 2)")
                .expect("supported")
                .is_err()
        );
        assert!(
            parse_typed_property("width", "/**/1/**/px")
                .expect("supported")
                .is_err()
        );
    }

    #[test]
    fn applies_property_specific_ranges_and_dimensions() {
        assert!(
            parse_typed_property("padding-left", "-1px")
                .expect("supported")
                .is_err()
        );
        assert!(
            parse_typed_property("border-left-width", "10%")
                .expect("supported")
                .is_err()
        );
        assert!(
            parse_typed_property("margin-left", "-10%")
                .expect("supported")
                .is_ok()
        );
        assert_eq!(parse("opacity", "150%").to_css(), "1");
        assert_eq!(parse("opacity", "-0.5").to_css(), "0");
        assert_eq!(parse("opacity", "calc(2 * 25%)").to_css(), "0.5");
        assert_eq!(parse("opacity", "min(80%, 0% + 50%)").to_css(), "0.5");
    }

    #[test]
    fn parses_intrinsic_size_keywords_without_confusing_max_none() {
        assert!(matches!(
            parse("width", "min-content"),
            TypedPropertyValue::Size(Size::MinContent)
        ));
        assert!(matches!(
            parse("max-width", "none"),
            TypedPropertyValue::MaxSize(MaxSize::None)
        ));
        assert_eq!(
            parse("width", "fit-content(calc(50% - 1px))").to_css(),
            "fit-content(calc(50% - 1px))"
        );
    }

    #[test]
    fn parses_named_hex_legacy_and_modern_srgb_colors() {
        assert_eq!(
            parse("color", "rebeccapurple").to_css(),
            "rgb(102, 51, 153)"
        );
        assert_eq!(
            parse("background-color", "#0f08").to_css(),
            "rgba(0, 255, 0, 0.53333336)"
        );
        assert_eq!(
            parse("background-image", "url(https://example.com/bg.png)").to_css(),
            "url(https://example.com/bg.png)"
        );
        assert_eq!(
            parse("color", "rgb(100%, 0%, 50%)").to_css(),
            "rgb(255, 0, 128)"
        );
        assert_eq!(
            parse("color", "rgb(10 20 30 / 25%)").to_css(),
            "rgba(10, 20, 30, 0.25)"
        );
    }

    #[test]
    fn parses_color_scheme_supports_and_resolves_used_scheme() {
        assert_eq!(
            parse("color-scheme", "normal"),
            TypedPropertyValue::ColorScheme(super::ColorSchemeValue::Normal)
        );
        assert_eq!(parse("color-scheme", "light dark").to_css(), "light dark");
        assert_eq!(parse("color-scheme", "only light").to_css(), "only light");
        assert_eq!(
            parse("color-scheme", "only dark light").to_css(),
            "only dark light"
        );
        for invalid in [
            "only",
            "normal light",
            "only only light",
            "light, dark",
            "inherit",
        ] {
            assert!(
                parse_typed_property("color-scheme", invalid)
                    .expect("color-scheme is supported")
                    .is_err(),
                "{invalid:?} must not parse"
            );
        }

        let normal = parse("color-scheme", "normal");
        let TypedPropertyValue::ColorScheme(normal) = normal else {
            unreachable!();
        };
        assert!(normal.used_dark(true));
        assert!(!normal.used_dark(false));

        let both = parse("color-scheme", "light dark");
        let TypedPropertyValue::ColorScheme(both) = both else {
            unreachable!();
        };
        assert!(both.used_dark(true));
        assert!(!both.used_dark(false));

        let only_dark = parse("color-scheme", "only dark");
        let TypedPropertyValue::ColorScheme(only_dark) = only_dark else {
            unreachable!();
        };
        assert!(only_dark.used_dark(false));

        let only_light = parse("color-scheme", "only light");
        let TypedPropertyValue::ColorScheme(only_light) = only_light else {
            unreachable!();
        };
        assert!(!only_light.used_dark(true));

        let custom = parse("color-scheme", "only my-scheme");
        let TypedPropertyValue::ColorScheme(custom) = custom else {
            unreachable!();
        };
        assert!(
            !custom.used_dark(true),
            "unknown schemes fall back to light"
        );
    }

    #[test]
    fn parses_content_visibility_keywords() {
        assert_eq!(
            parse("content-visibility", "hidden"),
            TypedPropertyValue::ContentVisibility(super::ContentVisibility::Hidden)
        );
        assert_eq!(parse("content-visibility", "auto").to_css(), "auto");
        assert!(
            parse_typed_property("content-visibility", "collapse")
                .expect("content-visibility is supported")
                .is_err()
        );
    }

    #[test]
    fn parses_direction_keywords() {
        assert_eq!(
            parse("direction", "rtl"),
            TypedPropertyValue::Direction(super::Direction::Rtl)
        );
        assert_eq!(parse("direction", "ltr").to_css(), "ltr");
        assert!(
            parse_typed_property("direction", "sideways")
                .expect("direction is supported")
                .is_err()
        );
    }

    #[test]
    fn parses_object_fit_keywords() {
        assert_eq!(parse("object-fit", "cover").to_css(), "cover");
        assert_eq!(parse("object-fit", "scale-down").to_css(), "scale-down");
        assert!(
            parse_typed_property("object-fit", "stretch")
                .expect("object-fit is supported")
                .is_err()
        );
    }

    #[test]
    fn parses_text_alignment_keywords() {
        assert_eq!(
            parse("text-align", "center"),
            TypedPropertyValue::TextAlign(TextAlign::Center)
        );
        assert_eq!(parse("text-align", "right").to_css(), "right");
        assert!(
            parse_typed_property("text-align", "middle")
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn resolves_typed_lengths_only_when_layout_supplies_the_basis() {
        let TypedPropertyValue::Size(Size::LengthPercentage(value)) =
            parse("width", "calc(50% - 2rem)")
        else {
            panic!("expected a typed size")
        };
        assert!(
            value
                .resolve(&super::LengthResolutionContext::default())
                .is_err()
        );
        let context = super::LengthResolutionContext {
            percentage_basis: Some(800.0),
            root_font_size: 16.0,
            ..super::LengthResolutionContext::default()
        };
        assert_eq!(value.resolve(&context), Ok(368.0));

        let TypedPropertyValue::Size(Size::LengthPercentage(inches)) = parse("width", "2in") else {
            panic!("expected a typed size")
        };
        assert_eq!(inches.resolve(&context), Ok(192.0));
    }

    #[test]
    fn parses_flex_longhands_and_expands_common_shorthands() {
        assert_eq!(
            parse("flex-direction", "column-reverse").to_css(),
            "column-reverse"
        );
        assert_eq!(parse("flex-grow", "2.5").to_css(), "2.5");
        assert_eq!(parse("flex-shrink", "0").to_css(), "0");
        assert_eq!(
            parse("flex-basis", "calc(50% - 2px)").to_css(),
            "calc(50% - 2px)"
        );
        assert_eq!(
            parse("justify-content", "space-evenly").to_css(),
            "space-evenly"
        );
        assert_eq!(parse("align-items", "stretch").to_css(), "stretch");
        assert_eq!(parse("order", "-3").to_css(), "-3");
        assert_eq!(parse("column-gap", "1rem").to_css(), "1rem");
        assert_eq!(parse("column-count", "auto").to_css(), "auto");
        assert_eq!(parse("column-count", "3").to_css(), "3");
        assert!(parse_typed_property("column-count", "0").unwrap().is_err());
        assert!(
            parse_typed_property("column-count", "2.5")
                .unwrap()
                .is_err()
        );
        assert_eq!(parse("column-width", "200px").to_css(), "200px");
        assert_eq!(parse("column-width", "0").to_css(), "0px");
        // CSS Multi-column §3.2: a percentage is not a column width.
        assert!(
            parse_typed_property("column-width", "50%")
                .unwrap()
                .is_err()
        );
        assert_eq!(parse("column-fill", "auto").to_css(), "auto");
        assert_eq!(parse("column-fill", "balance").to_css(), "balance");
        assert!(parse_typed_property("flex-grow", "-1").unwrap().is_err());
        assert!(parse_typed_property("order", "1.5").unwrap().is_err());
        assert_eq!(
            super::expand_gap_shorthand("10px 2rem"),
            Some(("10px".to_owned(), "2rem".to_owned()))
        );
        assert_eq!(
            super::expand_flex_shorthand("2 3 10%"),
            Some(("2".to_owned(), "3".to_owned(), "10%".to_owned()))
        );
        assert_eq!(
            super::expand_flex_shorthand("1"),
            Some(("1".to_owned(), "1".to_owned(), "0%".to_owned()))
        );
    }

    #[test]
    fn columns_shorthand_expands_into_column_width_and_column_count() {
        // CSS Multi-column §3: each longhand that is omitted is `auto`, and each
        // token is classified by the grammar it parses under.
        let expand = super::expand_columns_shorthand;
        assert_eq!(expand("2"), Some(("auto".to_owned(), "2".to_owned())));
        assert_eq!(
            expand("200px"),
            Some(("200px".to_owned(), "auto".to_owned()))
        );
        assert_eq!(
            expand("200px 3"),
            Some(("200px".to_owned(), "3".to_owned()))
        );
        assert_eq!(
            expand("3 200px"),
            Some(("200px".to_owned(), "3".to_owned()))
        );
        assert_eq!(expand("auto"), Some(("auto".to_owned(), "auto".to_owned())));
        // A repeated kind, or a token that is neither, makes the shorthand invalid.
        assert_eq!(expand("2 3"), None);
        assert_eq!(expand("50% 2"), None);
        assert_eq!(expand("2 foo"), None);
    }

    #[test]
    fn accepts_legacy_flex_display_values_used_by_163() {
        for value in ["-webkit-box", "-webkit-flex", "-ms-flexbox"] {
            assert_eq!(parse("display", value).to_css(), "flex", "{value}");
        }
    }

    #[test]
    fn parses_explicit_and_responsive_grid_track_lists() {
        assert_eq!(
            parse("grid-template-columns", "120px 25% 2fr").to_css(),
            "120px 25% 2fr"
        );
        assert_eq!(
            parse("grid-template-rows", "repeat(2, 40px 1fr)").to_css(),
            "40px 1fr 40px 1fr"
        );
        assert_eq!(
            parse(
                "grid-template-columns",
                "repeat(auto-fit, minmax(9rem, 1fr))"
            )
            .to_css(),
            "repeat(auto-fit, minmax(9rem, 1fr))"
        );
        for invalid in [
            "repeat(0, 1fr)",
            "repeat(5000, 1px)",
            "minmax(1fr, 10px)",
            "repeat(auto-fit, 1fr)",
        ] {
            assert!(
                parse_typed_property("grid-template-columns", invalid)
                    .expect("supported")
                    .is_err(),
                "unexpectedly accepted {invalid}"
            );
        }
    }

    #[test]
    fn grid_gap_legacy_alias_expands_to_row_and_column_gap() {
        let Some((row, column)) = super::expand_gap_shorthand("20px") else {
            panic!("single-value grid-gap must expand");
        };
        assert_eq!(row, "20px");
        assert_eq!(column, "20px");
        let Some((row, column)) = super::expand_gap_shorthand("8px 24px") else {
            panic!("two-value grid-gap must expand");
        };
        assert_eq!(row, "8px");
        assert_eq!(column, "24px");
    }

    #[test]
    fn parses_grid_line_longhands_with_span_and_negative_forms() {
        use super::GridLine;
        let line = |value: &str| match parse("grid-column-start", value) {
            TypedPropertyValue::GridLine(line) => line,
            other => panic!("expected a grid line, got {other:?}"),
        };
        assert_eq!(line("auto"), GridLine::Auto);
        assert_eq!(line("3"), GridLine::Line(3));
        assert_eq!(line("-2"), GridLine::Line(-2));
        assert_eq!(line("span"), GridLine::Span(1));
        assert_eq!(line("span 2"), GridLine::Span(2));
        assert_eq!(line("2 span"), GridLine::Span(2));
        assert_eq!(parse("grid-row-end", "span 4").to_css(), "span 4");
        for invalid in ["0", "span 0", "span -1", "-2 span", "2.5", "main-start", ""] {
            assert!(
                parse_typed_property("grid-column-start", invalid)
                    .expect("supported")
                    .is_err(),
                "unexpectedly accepted {invalid}"
            );
        }
    }

    #[test]
    fn grid_axis_shorthand_expands_start_and_end_lines() {
        let expand = |value: &str| super::expand_grid_axis_shorthand(value);
        assert_eq!(expand("2"), Some(("2".to_owned(), "auto".to_owned())));
        assert_eq!(
            expand("span 2 / 3"),
            Some(("span 2".to_owned(), "3".to_owned()))
        );
        assert_eq!(expand("1 / -1"), Some(("1".to_owned(), "-1".to_owned())));
        assert_eq!(
            expand("auto / span 2"),
            Some(("auto".to_owned(), "span 2".to_owned()))
        );
        // Two spans cannot resolve against a grid; the whole shorthand is
        // invalid (CSS Grid §8.1).
        assert_eq!(expand("span 2 / span 3"), None);
        assert_eq!(expand("1 / 0"), None);
        assert_eq!(expand("/ 2"), None);
        assert_eq!(expand("1 / 2 / 3"), None);
    }

    #[test]
    fn transform_none_is_an_empty_list_that_serializes_to_none() {
        assert_eq!(transform("none"), TransformList(Vec::new()));
        assert_eq!(transform("none").to_css(), "none");
    }

    #[test]
    fn parses_translate_functions_in_all_grammatical_forms() {
        assert_eq!(
            transform("translate(10px)").0,
            vec![TransformFunction::Translate(
                px(10.0),
                LengthPercentage::Zero
            )]
        );
        assert_eq!(
            transform("translate(10px, 20px)").0,
            vec![TransformFunction::Translate(px(10.0), px(20.0))]
        );
        assert_eq!(
            transform("translateX(-3px)").0,
            vec![TransformFunction::Translate(
                px(-3.0),
                LengthPercentage::Zero
            )]
        );
        assert_eq!(
            transform("translateY(25%)").0,
            vec![TransformFunction::Translate(
                LengthPercentage::Zero,
                LengthPercentage::Percentage(0.25)
            )]
        );
        assert_eq!(
            transform("translate3d(0, 0, 0)").0,
            vec![TransformFunction::Translate(
                LengthPercentage::Zero,
                LengthPercentage::Zero
            )]
        );
        assert!(matches!(
            transform("translate(calc(100% - 10px), 0)").0.as_slice(),
            [TransformFunction::Translate(
                LengthPercentage::Calculation(_),
                LengthPercentage::Zero
            )]
        ));
        // translateZ contributes nothing to the 2D pipeline.
        assert_eq!(transform("translateZ(10px)"), TransformList(Vec::new()));
    }

    #[test]
    fn parses_scale_functions_with_second_argument_defaulting() {
        assert_eq!(
            transform("scale(2)").0,
            vec![TransformFunction::Scale(2.0, 2.0)]
        );
        assert_eq!(
            transform("scale(2, 0.5)").0,
            vec![TransformFunction::Scale(2.0, 0.5)]
        );
        assert_eq!(
            transform("scale(-1)").0,
            vec![TransformFunction::Scale(-1.0, -1.0)]
        );
        assert_eq!(
            transform("scale(0)").0,
            vec![TransformFunction::Scale(0.0, 0.0)]
        );
        assert_eq!(
            transform("scaleX(3)").0,
            vec![TransformFunction::Scale(3.0, 1.0)]
        );
        assert_eq!(
            transform("scaleY(0.25)").0,
            vec![TransformFunction::Scale(1.0, 0.25)]
        );
        assert_eq!(
            transform("scale3d(2, 3, 4)").0,
            vec![TransformFunction::Scale(2.0, 3.0)]
        );
        // scaleZ contributes nothing to the 2D pipeline.
        assert_eq!(transform("scaleZ(2)"), TransformList(Vec::new()));
    }

    #[test]
    fn parses_rotate_angles_across_units_and_axis_aliases() {
        use std::f32::consts::{FRAC_PI_2, PI};
        assert_eq!(
            transform("rotate(45deg)").0,
            vec![TransformFunction::Rotate(45f32.to_radians())]
        );
        assert_eq!(
            transform("rotate(0)").0,
            vec![TransformFunction::Rotate(0.0)]
        );
        assert_eq!(
            transform("rotate(0.5turn)").0,
            vec![TransformFunction::Rotate(PI)]
        );
        assert_eq!(
            transform("rotate(200grad)").0,
            vec![TransformFunction::Rotate(PI)]
        );
        assert_eq!(
            transform("rotate(1rad)").0,
            vec![TransformFunction::Rotate(1.0)]
        );
        assert_eq!(
            transform("rotateZ(-90deg)").0,
            vec![TransformFunction::Rotate(-FRAC_PI_2)]
        );
        assert_eq!(
            transform("rotate(-90deg)").0,
            vec![TransformFunction::Rotate(-FRAC_PI_2)]
        );
        // Function names and angle units are ASCII case-insensitive.
        assert_eq!(
            transform("ROTATE(90DEG)").0,
            vec![TransformFunction::Rotate(FRAC_PI_2)]
        );
        assert_eq!(
            transform("rotate3d(0, 0, 1, 45deg)").0,
            vec![TransformFunction::Rotate(45f32.to_radians())]
        );
        // A negative z axis component flips the rotation direction.
        assert_eq!(
            transform("rotate3d(0, 0, -2, 45deg)").0,
            vec![TransformFunction::Rotate(-45f32.to_radians())]
        );
    }

    #[test]
    fn parses_skew_functions_with_optional_second_angle() {
        assert_eq!(
            transform("skew(10deg)").0,
            vec![TransformFunction::Skew(10f32.to_radians(), 0.0)]
        );
        assert_eq!(
            transform("skew(10deg, -20deg)").0,
            vec![TransformFunction::Skew(
                10f32.to_radians(),
                -20f32.to_radians()
            )]
        );
        assert_eq!(
            transform("skewX(30deg)").0,
            vec![TransformFunction::Skew(30f32.to_radians(), 0.0)]
        );
        assert_eq!(
            transform("skewY(45deg)").0,
            vec![TransformFunction::Skew(0.0, 45f32.to_radians())]
        );
    }

    #[test]
    fn parses_matrix_and_projects_affine_matrix3d() {
        assert_eq!(
            transform("matrix(1, 2, 3, 4, 5, 6)").0,
            vec![TransformFunction::Matrix([1.0, 2.0, 3.0, 4.0, 5.0, 6.0])]
        );
        assert_eq!(
            transform("matrix3d(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 10, 20, 0, 1)").0,
            vec![TransformFunction::Matrix([1.0, 0.0, 0.0, 1.0, 10.0, 20.0])]
        );
        // A perspective term (m14 != 0 here) cannot be projected onto the 2D
        // affine pipeline, so the declaration is dropped.
        assert!(
            parse_typed_property(
                "transform",
                "matrix3d(1, 0, 0, 1, 0, 1, 0, 0, 0, 0, 1, 0, 10, 20, 0, 1)"
            )
            .expect("supported")
            .is_err()
        );
    }

    #[test]
    fn rejects_3d_only_transform_functions() {
        for css in [
            "perspective(10px)",
            "rotateX(45deg)",
            "rotateY(45deg)",
            "rotate3d(1, 0, 0, 45deg)",
            "rotate3d(0, 1, 0, 45deg)",
            "rotate3d(0, 0, 0, 45deg)",
            "translateZ(10px) rotateX(45deg)",
        ] {
            assert!(
                parse_typed_property("transform", css)
                    .expect("supported")
                    .is_err(),
                "unexpectedly accepted {css}"
            );
        }
    }

    #[test]
    fn rejects_malformed_transform_values_with_a_source_location() {
        for css in [
            "",
            "10px",
            "bogus(10px)",
            "translate 10px",
            "translate()",
            "translate(10px,)",
            "translate(10px, 20px, 5px)",
            "translate(10px 20px)",
            "scale()",
            "scale(2,)",
            "scale(1e40)",
            "scale3d(2, 3)",
            "matrix(1, 2, 3)",
            "matrix(1, 2, 3, 4, 5, 6, 7)",
            "matrix(1e40, 0, 0, 1, 0, 0)",
            "rotate()",
            "rotate(45)",
            "rotate(1e40deg)",
            "skew(10deg, 20deg, 30deg)",
            "translateX(10px), rotate(45deg)",
            "translateX(10px) 10px",
        ] {
            assert!(
                parse_typed_property("transform", css)
                    .expect("supported")
                    .is_err(),
                "unexpectedly accepted {css:?}"
            );
        }
        // Errors carry the source location through PropertyParseError.
        let error = invalid("transform", "rotate(45)");
        assert_eq!(error.property(), "transform");
        assert!(error.to_string().contains("at 0:8"), "{error}");
    }

    #[test]
    fn preserves_transform_function_order_in_lists() {
        assert_eq!(
            transform("translateX(10px) rotate(45deg)").0,
            vec![
                TransformFunction::Translate(px(10.0), LengthPercentage::Zero),
                TransformFunction::Rotate(45f32.to_radians()),
            ]
        );
        assert_eq!(transform("scale(2) skewX(30deg) translate(5px)").0.len(), 3);
        // Identity-only 3D functions contribute nothing but keep their
        // neighbors' order intact.
        assert_eq!(
            transform("translateZ(10px) rotate(45deg) scaleZ(2)").0,
            vec![TransformFunction::Rotate(45f32.to_radians())]
        );
    }

    #[test]
    fn normalizes_transform_serialization() {
        assert_eq!(
            transform("translate(10px)").to_css(),
            "translate(10px, 0px)"
        );
        assert_eq!(transform("translateX(5%)").to_css(), "translate(5%, 0px)");
        assert_eq!(
            transform("translateY(-2px)").to_css(),
            "translate(0px, -2px)"
        );
        assert_eq!(
            transform("translate(calc(100% - 10px), 0)").to_css(),
            "translate(calc(100% - 10px), 0px)"
        );
        assert_eq!(
            transform("translate3d(0, 0, 0)").to_css(),
            "translate(0px, 0px)"
        );
        assert_eq!(transform("scale(2)").to_css(), "scale(2, 2)");
        assert_eq!(transform("scaleX(3)").to_css(), "scale(3, 1)");
        assert_eq!(transform("rotate(0.25turn)").to_css(), "rotate(90deg)");
        assert_eq!(transform("rotate(200grad)").to_css(), "rotate(180deg)");
        // Radian storage snaps back to clean degrees on serialization.
        assert_eq!(transform("rotate(1rad)").to_css(), "rotate(57.29578deg)");
        assert_eq!(transform("rotate(-90deg)").to_css(), "rotate(-90deg)");
        assert_eq!(transform("skew(30deg)").to_css(), "skew(30deg, 0deg)");
        assert_eq!(transform("skewY(45deg)").to_css(), "skew(0deg, 45deg)");
        assert_eq!(
            transform("matrix(1, 0, 0, 1, 10, 20)").to_css(),
            "matrix(1, 0, 0, 1, 10, 20)"
        );
        assert_eq!(
            transform("translateX(10px) rotate(45deg)").to_css(),
            "translate(10px, 0px) rotate(45deg)"
        );
    }

    #[test]
    fn parses_transform_origin_keywords_lengths_and_defaults() {
        assert_eq!(
            origin("50% 50%"),
            TransformOrigin(
                LengthPercentage::Percentage(0.5),
                LengthPercentage::Percentage(0.5)
            )
        );
        assert_eq!(
            origin("left top"),
            TransformOrigin(
                LengthPercentage::Percentage(0.0),
                LengthPercentage::Percentage(0.0)
            )
        );
        assert_eq!(origin("100px 200px"), TransformOrigin(px(100.0), px(200.0)));
        // Single values leave the missing slot at the 50% default.
        assert_eq!(
            origin("center"),
            TransformOrigin(
                LengthPercentage::Percentage(0.5),
                LengthPercentage::Percentage(0.5)
            )
        );
        assert_eq!(
            origin("left"),
            TransformOrigin(
                LengthPercentage::Percentage(0.0),
                LengthPercentage::Percentage(0.5)
            )
        );
        assert_eq!(
            origin("top"),
            TransformOrigin(
                LengthPercentage::Percentage(0.5),
                LengthPercentage::Percentage(0.0)
            )
        );
        assert_eq!(
            origin("center bottom"),
            TransformOrigin(
                LengthPercentage::Percentage(0.5),
                LengthPercentage::Percentage(1.0)
            )
        );
        assert_eq!(
            origin("10px"),
            TransformOrigin(px(10.0), LengthPercentage::Percentage(0.5))
        );
    }

    #[test]
    fn parses_transform_origin_with_leading_vertical_keyword() {
        // Regression: `top`/`bottom` first used to fall through to an error.
        assert_eq!(
            origin("top left"),
            TransformOrigin(
                LengthPercentage::Percentage(0.0),
                LengthPercentage::Percentage(0.0)
            )
        );
        assert_eq!(
            origin("top center"),
            TransformOrigin(
                LengthPercentage::Percentage(0.5),
                LengthPercentage::Percentage(0.0)
            )
        );
        assert_eq!(
            origin("bottom right"),
            TransformOrigin(
                LengthPercentage::Percentage(1.0),
                LengthPercentage::Percentage(1.0)
            )
        );
        assert_eq!(
            origin("top 25%"),
            TransformOrigin(
                LengthPercentage::Percentage(0.25),
                LengthPercentage::Percentage(0.0)
            )
        );
        assert_eq!(
            origin("bottom 10px"),
            TransformOrigin(px(10.0), LengthPercentage::Percentage(1.0))
        );
    }

    #[test]
    fn parses_transform_origin_three_value_forms_ignoring_z() {
        assert_eq!(
            origin("50% 50% 10px"),
            TransformOrigin(
                LengthPercentage::Percentage(0.5),
                LengthPercentage::Percentage(0.5)
            )
        );
        assert_eq!(
            origin("left top 0"),
            TransformOrigin(
                LengthPercentage::Percentage(0.0),
                LengthPercentage::Percentage(0.0)
            )
        );
    }

    #[test]
    fn serializes_transform_origin_normally() {
        assert_eq!(origin("left top").to_css(), "0% 0%");
        assert_eq!(origin("center").to_css(), "50% 50%");
        assert_eq!(origin("100px 200px").to_css(), "100px 200px");
    }

    #[test]
    fn rejects_invalid_transform_origin_values() {
        for css in [
            "",
            "top top",
            "left left",
            "center left",
            "left top 25%",
            "top center right",
            "top 10px left",
            "left 10px top",
            "middle",
        ] {
            assert!(
                parse_typed_property("transform-origin", css)
                    .expect("supported")
                    .is_err(),
                "unexpectedly accepted {css:?}"
            );
        }
    }

    /// CSS Color 4 §10.1: the two sRGB spaces are taken as written, and §10.4's
    /// Display P3 is converted to sRGB by the §10.12 algorithm.
    #[test]
    fn parses_color_in_srgb_and_display_p3() {
        assert_eq!(srgb("color(srgb 1 0 0)"), (255, 0, 0, 1.0));
        assert_eq!(srgb("color(srgb 100% 0% 0% / 50%)"), (255, 0, 0, 0.5));
        // §4.4: a missing component is a zero, including when converting.
        assert_eq!(srgb("color(srgb none 0 0)"), (0, 0, 0, 1.0));
        // The primaries round-trip through the sRGB transfer function.
        assert_eq!(srgb("color(srgb 0 1 0)"), (0, 255, 0, 1.0));
        assert_eq!(srgb("color(srgb 0 0 1)"), (0, 0, 255, 1.0));
        // `srgb-linear` 0.5 is the gamma-encoded 0.735, so ~188.
        let (red, green, blue, alpha) = srgb("color(srgb-linear 0.5 0 0)");
        assert_eq!((red, green, blue, alpha), (188, 0, 0, 1.0));

        // The spec's own worked example, §2: a leaf green written in both sRGB
        // and Display P3 has to convert to the same color. It also proves the
        // matrices are right rather than merely plausible.
        assert_eq!(
            srgb("color(srgb 0.41587 0.50367 0.36664)"),
            srgb("color(display-p3 0.43313 0.50108 0.3795)")
        );
        // Display P3 white is sRGB white, since both share the D65 white point
        // and §10.12 skips chromatic adaptation when the white points agree.
        assert_eq!(srgb("color(display-p3 1 1 1)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("color(display-p3 0 0 0)"), (0, 0, 0, 1.0));
        // The three saturated colors a real page ships as Display P3. Each is
        // outside the sRGB gamut; local-MINDE mapping accepts the channel-clipped
        // form because it stays within one JND (§14.2.2), so these pin the
        // conversion and the 8-bit device boundary together.
        assert_eq!(
            srgb("color(display-p3 .15546 .38118 .86881)"),
            (0, 99, 230, 1.0)
        );
        assert_eq!(
            srgb("color(display-p3 .25253 .6243 .39945)"),
            (0, 162, 96, 1.0)
        );
        assert_eq!(
            srgb("color(display-p3 .07412 .21127 .49921)"),
            (0, 55, 132, 1.0)
        );
        // A Display P3 primary is not the sRGB primary of the same name, which
        // is what would happen if the components were copied instead of
        // converted. §14.2's perceptual mapping reduces OKLCH chroma rather
        // than clipping each channel, so the green primary keeps a little red
        // and blue and the red primary keeps a little green and blue.
        assert_eq!(srgb("color(display-p3 1 0 0)"), (255, 11, 12, 1.0));
        assert_eq!(srgb("color(display-p3 0 1 0)"), (0, 251, 41, 1.0));
        assert_eq!(srgb("color(display-p3 0 0 1)"), (0, 0, 255, 1.0));
        assert_eq!(srgb("color(display-p3 1 1 0)"), (254, 255, 0, 1.0));
    }

    /// CSS Color 4 §4.4: `none` parses as a missing component in every modern
    /// color function, is preserved for interpolation/serialization, and still
    /// behaves as zero when the color is rendered directly.
    #[test]
    fn parses_missing_components_and_renders_them_as_zero() {
        match parse("color", "rgb(none 128 0 / none)") {
            TypedPropertyValue::Color(CssColor::Space(space)) => {
                assert_eq!(space.space, crate::color::ColorSpace::Srgb);
                assert_eq!(space.components[0], None);
                assert!((space.components[1].unwrap() - 128.0 / 255.0).abs() < 1e-6);
                assert_eq!(space.components[2], Some(0.0));
                assert_eq!(space.alpha, None);
                assert_eq!(space.to_css(), "color(srgb none 0.5019608 0 / none)");
            }
            other => panic!("expected a missing-component color, got {other:?}"),
        }
        let css = |value: &str| match parse("color", value) {
            TypedPropertyValue::Color(color) => color.to_css(),
            other => panic!("expected a color, got {other:?}"),
        };
        assert_eq!(css("hsl(none 100% 50%)"), "hsl(none 100% 50%)");
        assert_eq!(css("hwb(none 0% 50%)"), "hwb(none 0% 50%)");
        assert_eq!(css("lab(none 20 30 / none)"), "lab(none 20 30 / none)");
        assert_eq!(css("lch(none 20 30)"), "lch(none 20 30)");
        assert_eq!(css("oklab(none 0.1 0.2)"), "oklab(none 0.1 0.2)");
        assert_eq!(css("oklch(none 0.1 30 / 50%)"), "oklch(none 0.1 30 / 0.5)");
        assert_eq!(
            css("color(display-p3 none 0.5 0)"),
            "color(display-p3 none 0.5 0)"
        );

        // Rendering directly treats each missing component as zero (§4.4).
        assert_eq!(srgb("rgb(none 128 0 / 50%)"), (0, 128, 0, 0.5));
        assert_eq!(srgb("rgb(none 128 0 / none)"), (0, 128, 0, 0.0));
        assert_eq!(srgb("hsl(none 100% 50%)"), (255, 0, 0, 1.0));
        assert_eq!(srgb("hsl(0 100% 50% / none)"), (255, 0, 0, 0.0));
        assert_eq!(srgb("hwb(none 0% 0%)"), (255, 0, 0, 1.0));
        assert_eq!(srgb("lab(none 0 0)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("lch(none 0 0)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("oklab(none 0 0)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("oklch(none 0 0)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("color(srgb none 0 0)"), (0, 0, 0, 1.0));

        // Legacy comma syntax may not use `none` (§4.1.2).
        for invalid in [
            "rgb(none, 0, 0)",
            "rgba(0, 0, 0, none)",
            "hsl(none, 100%, 50%)",
            "hsla(0, 100%, 50%, none)",
        ] {
            assert!(
                parse_typed_property("color", invalid)
                    .expect("supported property")
                    .is_err(),
                "{invalid:?} must not parse"
            );
        }
    }

    /// CSS Color 4 §13.3: analogous components carry missing state across
    /// color spaces before interpolation, then fill from the other color.
    #[test]
    fn color_mix_carries_missing_components_forward() {
        // Missing OKLCH lightness is carried from the other color, so L is
        // 0.8 rather than the average of 0 and 0.8. C=0.1875, H=45.
        assert_eq!(
            srgb("color-mix(in oklch, oklch(none 0.25 30), oklch(0.8 0.125 60))"),
            (255, 157, 105, 1.0)
        );
        // A missing HSL hue is analogous to the other HSL hue.
        assert_eq!(
            srgb("color-mix(in hsl, hsl(none 100% 50%), hsl(240 100% 50%))"),
            (0, 0, 255, 1.0)
        );
        // Missing on both sides stays missing and serializes as `none`.
        match parse(
            "color",
            "color-mix(in srgb, rgb(none 128 0), rgb(none 0 255))",
        ) {
            TypedPropertyValue::Color(CssColor::Space(space)) => {
                assert_eq!(space.components[0], None);
                assert!((space.components[1].unwrap() - 64.0 / 255.0).abs() < 1e-6);
                assert!((space.components[2].unwrap() - 0.5).abs() < 1e-6);
                assert_eq!(space.to_css(), "color(srgb none 0.2509804 0.5)");
            }
            other => panic!("expected a missing-component mix, got {other:?}"),
        }
        // Lab's a/b opponents form an analogous set with LCH chroma and hue.
        assert_eq!(
            srgb("color-mix(in lch, lab(50 none none), lch(50 40 120))"),
            (99, 128, 56, 1.0)
        );
        let css = match parse(
            "color",
            "color-mix(in hsl, hsl(none 0% 100%), hsl(none 0% 0%))",
        ) {
            TypedPropertyValue::Color(color) => color.to_css(),
            other => panic!("expected a color, got {other:?}"),
        };
        assert_eq!(css, "hsl(none 0% 50%)");
    }

    /// Every predefined `color()` space the engine can convert is accepted,
    /// and an unknown space is rejected with a diagnostic that names it.
    #[test]
    fn accepts_every_predefined_color_space_and_rejects_unknown_ones() {
        for css in [
            "color(rec2020 1 0 0)",
            "color(a98-rgb 1 0 0)",
            "color(prophoto-rgb 1 0 0)",
            "color(xyz 0.1 0.2 0.3)",
            "color(xyz-d50 0.1 0.2 0.3)",
            "color(xyz-d65 0.1 0.2 0.3)",
        ] {
            parse_typed_property("color", css)
                .expect("supported property")
                .unwrap_or_else(|error| panic!("{css} must parse: {error:?}"));
        }
        for (css, space) in [
            ("color(display-p3-linear 1 0 0)", "display-p3-linear"),
            ("color(not-a-space 1 0 0)", "not-a-space"),
            ("color(oklab 1 0 0)", "oklab"),
        ] {
            let error = parse_typed_property("color", css)
                .expect("supported property")
                .expect_err("must be rejected");
            assert_eq!(error.property(), "color");
            let detail = error.detail().unwrap_or_default();
            assert!(detail.contains(space), "{css}: {detail}");
        }
    }

    /// CSS Color 4 §4.4: `hwb()`, `lab()`/`lch()` and `oklab()`/`oklch()`
    /// compute to the equivalent sRGB color.
    #[test]
    fn resolves_hwb_lab_lch_oklab_and_oklch_to_srgb() {
        assert_eq!(srgb("hwb(0 0% 0%)"), (255, 0, 0, 1.0));
        assert_eq!(srgb("hwb(120 0% 0%)"), (0, 255, 0, 1.0));
        assert_eq!(srgb("hwb(0 100% 0%)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("hwb(0 0% 100%)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("hwb(0 50% 50%)"), (128, 128, 128, 1.0));
        assert_eq!(srgb("lab(100 0 0)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("lab(0 0 0)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("lch(100 0 0)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("lch(0 0 0)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("oklab(1 0 0)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("oklab(0 0 0)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("oklch(1 0 0)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("oklch(0 0 0)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("oklch(0.7 0.1 120)"), (150, 168, 94, 1.0));
        assert_eq!(srgb("oklch(70% 25% 120deg)"), (150, 168, 94, 1.0));
        assert_eq!(srgb("color(rec2020 1 1 1)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("color(a98-rgb 0 0 0)"), (0, 0, 0, 1.0));
        assert_eq!(srgb("color(prophoto-rgb 1 1 1)"), (255, 255, 255, 1.0));
        assert_eq!(srgb("color(xyz-d65 0 0 0)"), (0, 0, 0, 1.0));
        // `/ <alpha-value>` applies to every modern color function.
        assert_eq!(srgb("lab(50 0 0 / 50%)").3, 0.5);
        assert_eq!(srgb("oklch(50% 0.1 240 / 25%)").3, 0.25);
    }

    /// CSS Color 5 §3: `color-mix()` interpolates in the named space and
    /// resolves to sRGB when both operands are absolute.
    #[test]
    fn parses_color_mix_and_resolves_absolute_mixes() {
        assert_eq!(srgb("color-mix(in srgb, red, blue)"), (128, 0, 128, 1.0));
        assert_eq!(
            srgb("color-mix(in srgb, #ff0000 25%, #0000ff)"),
            (64, 0, 191, 1.0)
        );
        // Premultiplied alpha: a transparent operand halves the result's
        // alpha but keeps the opaque operand's hue.
        assert_eq!(
            srgb("color-mix(in srgb, red, transparent)"),
            (255, 0, 0, 0.5)
        );
        // Both percentages below 100% multiply the alpha by their sum.
        assert_eq!(
            srgb("color-mix(in srgb, red 25%, blue 25%)"),
            (128, 0, 128, 0.5)
        );
        // Above 100% the weights normalize and alpha stays opaque.
        assert_eq!(
            srgb("color-mix(in srgb, red 75%, blue 75%)"),
            (128, 0, 128, 1.0)
        );
        // Linear-light interpolation is not the same as gamma-space mixing.
        assert_eq!(
            srgb("color-mix(in srgb-linear, red, blue)"),
            (188, 0, 188, 1.0)
        );
        // The CSS Color 5 default space is oklab; check the actual result.
        assert_eq!(srgb("color-mix(in oklab, red, blue)"), (140, 83, 162, 1.0));
        // Shorter hue path goes through magenta, the longer path through green.
        assert_eq!(srgb("color-mix(in hsl, red, blue)"), (255, 0, 255, 1.0));
        assert_eq!(
            srgb("color-mix(in hsl longer, red, blue)"),
            (0, 255, 0, 1.0)
        );
        // Nested mixes resolve recursively.
        assert_eq!(
            srgb("color-mix(in srgb, color-mix(in srgb, red, blue), transparent)"),
            (128, 0, 128, 0.5)
        );
        // A mix result outside sRGB is gamut mapped at the device boundary,
        // not channel-clipped: the midpoint of the out-of-gamut Display P3
        // primaries is (78, 98, 0) perceptually, while clipping gives (74, 101, 0).
        assert_eq!(
            srgb("color-mix(in srgb, color(display-p3 1 0 0), color(display-p3 0 1 0))"),
            (78, 98, 0, 1.0)
        );
    }

    #[test]
    fn color_mix_with_currentcolor_stays_unresolved_until_paint() {
        match parse(
            "background-color",
            "color-mix(in srgb, currentcolor 20%, transparent)",
        ) {
            TypedPropertyValue::Color(CssColor::Mix(mix)) => {
                assert_eq!(mix.left_weight, 0.2);
                assert_eq!(mix.right_weight, 0.8);
                assert_eq!(mix.alpha_multiplier, 1.0);
                assert!(mix.to_css().contains("currentcolor 20%"));
            }
            other => panic!("expected an unresolved color mix, got {other:?}"),
        }
    }

    #[test]
    fn relative_colors_use_origin_components_and_calc() {
        // Channel keywords read the origin in the function's processing space.
        assert_eq!(srgb("rgb(from rgb(10 20 30) r g b)"), (10, 20, 30, 1.0));
        // A direct number in rgb() is the 0..255 form, not a percentage.
        assert_eq!(srgb("rgb(from rgb(10 20 30) 255 g b)"), (255, 20, 30, 1.0));
        assert_eq!(
            srgb("rgb(from rgb(10 20 30) calc(r + 10) g b)"),
            (20, 20, 30, 1.0)
        );
        // The spec's grayscale approximation: red * .3 + green * .59 + blue * .11.
        assert_eq!(
            srgb(
                "rgb(from red calc(r * .3 + g * .59 + b * .11) \
                 calc(r * .3 + g * .59 + b * .11) calc(r * .3 + g * .59 + b * .11))"
            ),
            (77, 77, 77, 1.0)
        );
        assert_eq!(
            srgb("hsl(from hsl(120 100% 50%) calc(h + 180) s l)"),
            (255, 0, 255, 1.0)
        );
        assert_eq!(
            srgb("oklch(from oklch(0.7 0.1 120) l c h)"),
            (150, 168, 94, 1.0)
        );
        assert_eq!(
            srgb("color(from color(srgb 0.1 0.2 0.3) srgb calc(r + 0.1) g b)"),
            (51, 51, 77, 1.0)
        );
        assert_eq!(srgb("rgb(from red r g b / 50%)"), (255, 0, 0, 0.5));
    }

    #[test]
    fn relative_colors_carry_missing_components() {
        match parse("color", "oklch(from hsl(none 100% 50%) l c h)") {
            TypedPropertyValue::Color(CssColor::Space(space)) => {
                assert_eq!(space.space, crate::color::ColorSpace::Oklch);
                assert!(space.components[2].is_none(), "hue must stay missing");
            }
            other => panic!("expected a missing-component space color, got {other:?}"),
        }
    }

    #[test]
    fn relative_colors_with_context_origins_stay_unresolved() {
        match parse("color", "rgb(from currentcolor r g b)") {
            TypedPropertyValue::Color(CssColor::Relative(relative)) => {
                assert_eq!(relative.function, crate::color::RelativeFunction::Rgb);
                assert!(relative.to_css().contains("currentcolor"));
            }
            other => panic!("expected an unresolved relative color, got {other:?}"),
        }
    }

    #[test]
    fn alpha_function_replaces_or_modifies_the_origin_alpha() {
        assert_eq!(
            srgb("alpha(from rgb(0 0 255 / 0.4) / 80%)"),
            (0, 0, 255, 0.8)
        );
        assert_eq!(
            srgb("alpha(from red / calc(alpha * 0.5))"),
            (255, 0, 0, 0.5)
        );
        assert_eq!(srgb("alpha(from rgb(1 2 3 / none) / none)"), (1, 2, 3, 0.0));
        match parse("color", "alpha(from currentcolor / 0.5)") {
            TypedPropertyValue::Color(CssColor::Alpha(alpha)) => {
                assert!(alpha.to_css().contains("currentcolor"));
            }
            other => panic!("expected an unresolved alpha color, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_alpha_functions() {
        for invalid in [
            "alpha(red)",
            "alpha(from red)",
            "alpha(from red /)",
            "alpha(from red / 50% 2)",
            "alpha(from red / r)",
        ] {
            assert!(
                parse_typed_property("color", invalid)
                    .expect("supported property")
                    .is_err(),
                "{invalid:?} must not parse"
            );
        }
    }

    #[test]
    fn light_dark_parses_both_arms() {
        match parse("color", "light-dark(rgb(1 2 3), rgb(4 5 6))") {
            TypedPropertyValue::Color(CssColor::LightDark(color)) => {
                assert_eq!(color.to_css(), "light-dark(rgb(1, 2, 3), rgb(4, 5, 6))");
            }
            other => panic!("expected light-dark, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_relative_colors() {
        for invalid in [
            "rgb(from red r g)",
            "rgb(from red r g b 0)",
            "rgb(from red r, g, b)",
            "rgb(from red h s l)",
            "hsl(from red h,s,l)",
            "oklch(from red l c 50%)",
            "oklch(from red l c calc(50%))",
            "rgb(from red calc(r + ) g b)",
            "light-dark(red)",
            "light-dark(red, blue, green)",
        ] {
            assert!(
                parse_typed_property("color", invalid)
                    .expect("supported property")
                    .is_err(),
                "{invalid:?} must not parse"
            );
        }
    }

    #[test]
    fn rejects_invalid_modern_color_functions_and_color_mix() {
        for invalid in [
            "hwb(0 0 0)",
            "hwb(0 0%)",
            "lab(100 0)",
            "lch(50 20)",
            "oklab(1 0)",
            "oklch(1 0.1)",
            "color-mix(red, blue)",
            "color-mix(in srgb red, blue)",
            "color-mix(in srgb, red blue)",
            "color-mix(in srgb, red 101%, blue)",
            "color-mix(in srgb, red -1%, blue)",
            "color-mix(in unknown, red, blue)",
            "color-mix(in srgb longer, red, blue)",
            "color-mix(in oklch unknown, red, blue)",
            "color-mix(in srgb, red, blue, green)",
        ] {
            assert!(
                parse_typed_property("color", invalid)
                    .expect("supported property")
                    .is_err(),
                "{invalid:?} must not parse"
            );
        }
    }

    #[test]
    fn rejects_malformed_color_function_values() {
        for css in [
            "",
            "color()",
            "color(srgb 1 0)",
            "color(srgb 1 0 0 0)",
            "color(srgb 1, 0, 0)",
            "color(srgb 1 0 0 /)",
            "color(srgb 1 0 0 / 50% 2)",
        ] {
            assert!(
                parse_typed_property("color", css)
                    .expect("supported")
                    .is_err(),
                "unexpectedly accepted {css:?}"
            );
        }
    }

    /// CSS 2.1 §17.6.1: one length sets both components, two lengths set the
    /// horizontal and vertical ones, and the spacing is never negative.
    #[test]
    fn parses_border_spacing_in_both_forms() {
        assert_eq!(parse("border-spacing", "0").to_css(), "0px");
        assert_eq!(parse("border-spacing", "2px").to_css(), "2px");
        assert_eq!(parse("border-spacing", "1px 2px").to_css(), "1px 2px");
        // A percentage is not in the CSS 2.1 grammar but is what HTML's
        // `cellspacing` presentational hint produces, and layout resolves it.
        assert_eq!(parse("border-spacing", "5%").to_css(), "5%");
        for invalid in ["-1px", "1px -2px", "", "auto", "1px 2px 3px"] {
            assert!(
                parse_typed_property("border-spacing", invalid)
                    .expect("supported")
                    .is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    /// CSS 2.1 §17.5.3 lists the eight alignment keywords; §10.8.3 adds the
    /// `<length>`/`<percentage>` offset form.
    #[test]
    fn parses_vertical_align_keywords_and_offsets() {
        for keyword in [
            "baseline",
            "sub",
            "super",
            "text-top",
            "text-bottom",
            "middle",
            "top",
            "bottom",
        ] {
            assert_eq!(parse("vertical-align", keyword).to_css(), keyword);
            // Keywords are ASCII case-insensitive per CSS Syntax §3.3.
            assert_eq!(
                parse("vertical-align", &keyword.to_ascii_uppercase()).to_css(),
                keyword
            );
        }
        assert_eq!(parse("vertical-align", "3px").to_css(), "3px");
        assert_eq!(parse("vertical-align", "50%").to_css(), "50%");
        assert_eq!(parse("vertical-align", "-2px").to_css(), "-2px");
        assert_eq!(
            parse("vertical-align", "calc(1em + 2px)").to_css(),
            "calc(1em + 2px)"
        );
        for invalid in ["", "flex-start", "center", "2"] {
            assert!(
                parse_typed_property("vertical-align", invalid)
                    .expect("supported")
                    .is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    /// Text Decoration 4 §2.1: `none`, or one or more line keywords combined
    /// with `||`. The accumulating form is the reason this cannot be a plain
    /// keyword type.
    #[test]
    fn parses_text_decoration_line_as_a_combining_set() {
        for value in [
            "none",
            "underline",
            "overline",
            "line-through",
            "blink",
            "spelling-error",
            "grammar-error",
            "underline overline",
            "underline line-through blink",
        ] {
            assert_eq!(
                parse("text-decoration-line", value).to_css(),
                value,
                "{value}"
            );
        }
        // `||` means each component at most once.
        for invalid in [
            "underline underline",
            "none underline",
            "underline none",
            "",
        ] {
            assert!(
                parse_typed_property("text-decoration-line", invalid)
                    .expect("supported")
                    .is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    /// Text Decoration 4 §2.2 and §2.4: single keywords, plus the
    /// `<length-percentage>` and `<line-width>` forms of the thickness.
    #[test]
    fn parses_text_decoration_style_and_thickness() {
        for value in ["solid", "double", "dotted", "dashed", "wavy"] {
            assert_eq!(
                parse("text-decoration-style", value).to_css(),
                value,
                "{value}"
            );
        }
        assert!(
            parse_typed_property("text-decoration-style", "underline")
                .expect("supported")
                .is_err(),
            "a line keyword is not a style; it would silently clear the line"
        );

        // `<length-percentage>` and `<line-width>` normalise, as lengths do
        // everywhere else in this crate.
        for (value, expected) in [
            ("auto", "auto"),
            ("from-font", "from-font"),
            ("thin", "thin"),
            ("medium", "medium"),
            ("thick", "thick"),
            ("2px", "2px"),
            ("0", "0px"),
            ("50%", "50%"),
        ] {
            assert_eq!(
                parse("text-decoration-thickness", value).to_css(),
                expected,
                "{value}"
            );
        }
        assert!(
            parse_typed_property("text-decoration-thickness", "wavy")
                .expect("supported")
                .is_err()
        );
    }

    /// Text Decoration 4 §2.3: `<color>` with `currentcolor` as the initial
    /// value, so the keyword has to be in the grammar.
    #[test]
    fn parses_text_decoration_color_including_currentcolor() {
        assert_eq!(
            parse("text-decoration-color", "currentcolor").to_css(),
            "currentcolor"
        );
        // A named colour serialises to its rgb() form, as it does for `color`.
        assert_eq!(
            parse("text-decoration-color", "red").to_css(),
            "rgb(255, 0, 0)"
        );
        assert!(
            parse_typed_property("text-decoration-color", "underline")
                .expect("supported")
                .is_err()
        );
    }

    /// The four keyword-only table properties, so layout never has to
    /// string-match a computed value.
    #[test]
    fn parses_the_keyword_only_table_properties() {
        let table_keywords: [(&str, &[&str]); 4] = [
            ("border-collapse", &["separate", "collapse"]),
            ("table-layout", &["auto", "fixed"]),
            ("empty-cells", &["show", "hide"]),
            (
                "caption-side",
                &["top", "bottom", "inline-start", "inline-end"],
            ),
        ];
        for (property, values) in table_keywords {
            for &value in values {
                assert_eq!(parse(property, value).to_css(), value, "{property}");
            }
            assert!(
                parse_typed_property(property, "inherit")
                    .expect("supported")
                    .is_err(),
                "{property} must not be a bare keyword type; the cascade resolves \
                 CSS-wide keywords before this stage"
            );
        }
        assert!(
            parse_typed_property("border-collapse", "collapsed")
                .expect("supported")
                .is_err()
        );
        assert!(
            parse_typed_property("caption-side", "inline")
                .expect("supported")
                .is_err()
        );
    }

    /// Every logical box longhand has a grammar, and every one maps to exactly
    /// the physical side CSS Logical Properties 1 assigns it in `horizontal-tb`.
    #[test]
    fn logical_box_longhands_parse_and_map_to_physical_sides() {
        use super::{Direction, LOGICAL_LONGHANDS, physical_longhand_for_logical};

        assert_eq!(LOGICAL_LONGHANDS.len(), 24);
        for &(logical, ltr_target, rtl_target) in LOGICAL_LONGHANDS {
            assert_eq!(
                physical_longhand_for_logical(logical, Direction::Ltr),
                Some(ltr_target),
                "{logical} ltr"
            );
            assert_eq!(
                physical_longhand_for_logical(logical, Direction::Rtl),
                Some(rtl_target),
                "{logical} rtl"
            );
            let value = if logical.ends_with("-style") {
                "solid"
            } else if logical.ends_with("-color") {
                "red"
            } else {
                "1px"
            };
            assert_eq!(parse(logical, value).kind(), {
                if logical.starts_with("border-") {
                    if logical.ends_with("-style") {
                        "border-style"
                    } else if logical.ends_with("-color") {
                        "color"
                    } else {
                        "border-width"
                    }
                } else if logical.starts_with("inset-") {
                    "inset"
                } else if logical.starts_with("padding-") {
                    "padding"
                } else {
                    "margin"
                }
            });
        }

        // The families accept their property-specific value grammars rather
        // than a shared "any length" fallback.
        assert_eq!(parse("margin-inline-start", "auto").to_css(), "auto");
        assert_eq!(parse("inset-block-end", "calc(10% + 2px)").kind(), "inset");
        assert_eq!(parse("border-inline-end-width", "thick").to_css(), "thick");
        assert_eq!(
            parse("border-block-start-style", "dashed").to_css(),
            "dashed"
        );
        assert_eq!(
            parse("border-inline-start-color", "red").to_css(),
            "rgb(255, 0, 0)"
        );
        assert!(
            parse_typed_property("padding-inline-end", "-1px")
                .expect("supported")
                .is_err(),
            "padding cannot be negative"
        );
    }
}
