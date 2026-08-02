//! Typed parsers for layout-facing computed CSS properties.
//!
//! This module deliberately parses the boundary-preserving serialization from
//! [`super::computed::ComputedValue`]. Percentages and mixed `calc()` trees are
//! retained until layout supplies the appropriate containing-block basis.

use std::error::Error;
use std::fmt;

use cssparser::color::{parse_hash_color, parse_named_color};
use cssparser::{ParseError, Parser, ParserInput, Token};

type CssResult<'i, T> = Result<T, ParseError<'i, ()>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyParseError {
    property: String,
    line: u32,
    column: u32,
}

impl PropertyParseError {
    #[must_use]
    pub fn property(&self) -> &str {
        &self.property
    }
}

impl fmt::Display for PropertyParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid computed value for '{}' at {}:{}",
            self.property, self.line, self.column
        )
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

fn format_number(value: f32) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

fn format_number_unit(value: f32, unit: &str) -> String {
    format!("{}{unit}", format_number(value))
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
keyword_enum!(FlexDirection {
    Row => "row",
    RowReverse => "row-reverse",
    Column => "column",
    ColumnReverse => "column-reverse",
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

#[derive(Clone, Debug, PartialEq)]
pub enum Size {
    Auto,
    MinContent,
    MaxContent,
    Stretch,
    FitContent(Option<LengthPercentage>),
    LengthPercentage(LengthPercentage),
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

#[derive(Clone, Copy, Debug, PartialEq)]
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
}

impl CssColor {
    #[must_use]
    pub fn to_css(self) -> String {
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
            } => format!("rgba({red}, {green}, {blue}, {})", format_number(alpha)),
            Self::CurrentColor => "currentcolor".to_owned(),
            Self::Canvas => "canvas".to_owned(),
            Self::CanvasText => "canvastext".to_owned(),
        }
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
    Opacity(f32),
    Size(Size),
    MaxSize(MaxSize),
    Inset(AutoLengthPercentage),
    Margin(AutoLengthPercentage),
    Padding(LengthPercentage),
    BorderWidth(BorderWidth),
    BorderStyle(BorderStyle),
    Color(CssColor),
    BackgroundImage(String),
    BackgroundRepeat(String),
    BackgroundPosition(String),
    BackgroundSize(String),
    FlexDirection(FlexDirection),
    FlexBasis(FlexBasis),
    FlexGrow(f32),
    FlexShrink(f32),
    JustifyContent(JustifyContent),
    AlignItems(AlignItems),
    Order(i32),
    Gap(Gap),
    GridTemplate(GridTemplate),
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
            Self::Opacity(_) => "opacity",
            Self::Size(_) => "size",
            Self::MaxSize(_) => "max-size",
            Self::Inset(_) => "inset",
            Self::Margin(_) => "margin",
            Self::Padding(_) => "padding",
            Self::BorderWidth(_) => "border-width",
            Self::BorderStyle(_) => "border-style",
            Self::Color(_) => "color",
            Self::BackgroundImage(_) => "background-image",
            Self::BackgroundRepeat(_) => "background-repeat",
            Self::BackgroundPosition(_) => "background-position",
            Self::BackgroundSize(_) => "background-size",
            Self::FlexDirection(_) => "flex-direction",
            Self::FlexBasis(_) => "flex-basis",
            Self::FlexGrow(_) => "flex-grow",
            Self::FlexShrink(_) => "flex-shrink",
            Self::JustifyContent(_) => "justify-content",
            Self::AlignItems(_) => "align-items",
            Self::Order(_) => "order",
            Self::Gap(_) => "gap",
            Self::GridTemplate(_) => "grid-template",
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
            Self::Opacity(value) | Self::FlexGrow(value) | Self::FlexShrink(value) => {
                format_number(*value)
            }
            Self::Size(value) | Self::MaxSize(MaxSize::Size(value)) => value.to_css(),
            Self::MaxSize(MaxSize::None) => "none".to_owned(),
            Self::Inset(value) | Self::Margin(value) => value.to_css(),
            Self::Padding(value) => value.to_css(),
            Self::BorderWidth(value) => value.to_css(),
            Self::BorderStyle(value) => value.as_str().to_owned(),
            Self::Color(value) => value.to_css(),
            Self::BackgroundImage(value) => value.clone(),
            Self::BackgroundRepeat(value)
            | Self::BackgroundPosition(value)
            | Self::BackgroundSize(value) => value.clone(),
            Self::FlexDirection(value) => value.as_str().to_owned(),
            Self::FlexBasis(value) => value.to_css(),
            Self::JustifyContent(value) => value.as_str().to_owned(),
            Self::AlignItems(value) => value.as_str().to_owned(),
            Self::Order(value) => value.to_string(),
            Self::Gap(value) => value.to_css(),
            Self::GridTemplate(value) => value.to_css(),
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

fn serialize_grid_tracks(tracks: &[GridTrack]) -> String {
    tracks
        .iter()
        .map(GridTrack::to_css)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a supported layout-facing property. `None` means that this slice does
/// not yet claim the property's grammar; `Some(Err(_))` means the property is
/// supported but the value is invalid at computed-value time.
#[must_use]
pub fn parse_typed_property(
    property: &str,
    css: &str,
) -> Option<Result<TypedPropertyValue, PropertyParseError>> {
    let supported = matches!(
        property,
        "display"
            | "color"
            | "background-color"
            | "background-image"
            | "background-repeat"
            | "background-position"
            | "background-size"
            | "position"
            | "float"
            | "clear"
            | "box-sizing"
            | "overflow-x"
            | "overflow-y"
            | "visibility"
            | "opacity"
            | "width"
            | "height"
            | "min-width"
            | "min-height"
            | "max-width"
            | "max-height"
            | "top"
            | "right"
            | "bottom"
            | "left"
            | "margin-top"
            | "margin-right"
            | "margin-bottom"
            | "margin-left"
            | "padding-top"
            | "padding-right"
            | "padding-bottom"
            | "padding-left"
            | "border-top-width"
            | "border-right-width"
            | "border-bottom-width"
            | "border-left-width"
            | "border-top-style"
            | "border-right-style"
            | "border-bottom-style"
            | "border-left-style"
            | "border-top-color"
            | "border-right-color"
            | "border-bottom-color"
            | "border-left-color"
            | "flex-direction"
            | "flex-basis"
            | "flex-grow"
            | "flex-shrink"
            | "justify-content"
            | "align-items"
            | "order"
            | "row-gap"
            | "column-gap"
            | "grid-template-columns"
            | "grid-template-rows"
    );
    if !supported {
        return None;
    }

    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let parsed = parser.parse_entirely(|input| parse_property(property, input));
    Some(parsed.map_err(|error| PropertyParseError {
        property: property.to_owned(),
        line: error.location.line,
        column: error.location.column,
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
        | "border-left-color" => parse_color(input).map(TypedPropertyValue::Color),
        "display" => parse_display(input).map(TypedPropertyValue::Display),
        "position" => parse_keyword(input, Position::parse).map(TypedPropertyValue::Position),
        "float" => parse_keyword(input, Float::parse).map(TypedPropertyValue::Float),
        "clear" => parse_keyword(input, Clear::parse).map(TypedPropertyValue::Clear),
        "box-sizing" => parse_keyword(input, BoxSizing::parse).map(TypedPropertyValue::BoxSizing),
        "overflow-x" | "overflow-y" => parse_overflow(input).map(TypedPropertyValue::Overflow),
        "visibility" => parse_keyword(input, Visibility::parse).map(TypedPropertyValue::Visibility),
        "opacity" => parse_opacity(input).map(TypedPropertyValue::Opacity),
        "flex-direction" => {
            parse_keyword(input, FlexDirection::parse).map(TypedPropertyValue::FlexDirection)
        }
        "flex-basis" => parse_flex_basis(input).map(TypedPropertyValue::FlexBasis),
        "flex-grow" => parse_non_negative_number(input).map(TypedPropertyValue::FlexGrow),
        "flex-shrink" => parse_non_negative_number(input).map(TypedPropertyValue::FlexShrink),
        "justify-content" => {
            parse_keyword(input, JustifyContent::parse).map(TypedPropertyValue::JustifyContent)
        }
        "align-items" => {
            parse_keyword(input, AlignItems::parse).map(TypedPropertyValue::AlignItems)
        }
        "order" => parse_integer(input).map(TypedPropertyValue::Order),
        "row-gap" | "column-gap" => parse_gap(input).map(TypedPropertyValue::Gap),
        "grid-template-columns" | "grid-template-rows" => {
            parse_grid_template(input).map(TypedPropertyValue::GridTemplate)
        }
        "width" | "height" | "min-width" | "min-height" => {
            parse_size(input).map(TypedPropertyValue::Size)
        }
        "max-width" | "max-height" => parse_max_size(input).map(TypedPropertyValue::MaxSize),
        "top" | "right" | "bottom" | "left" => {
            parse_auto_length_percentage(input, false).map(TypedPropertyValue::Inset)
        }
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            parse_auto_length_percentage(input, false).map(TypedPropertyValue::Margin)
        }
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
            parse_length_percentage(input, true).map(TypedPropertyValue::Padding)
        }
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            parse_border_width(input).map(TypedPropertyValue::BorderWidth)
        }
        "border-top-style" | "border-right-style" | "border-bottom-style" | "border-left-style" => {
            parse_keyword(input, BorderStyle::parse).map(TypedPropertyValue::BorderStyle)
        }
        _ => unreachable!("unsupported properties are filtered before parsing"),
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

fn parse_color<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, CssColor> {
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
            input.parse_nested_block(parse_rgb_color)
        }
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_background_image<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, String> {
    let location = input.current_source_location();
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

fn parse_rgb_color<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, CssColor> {
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
            parse_alpha_component(input)?
        } else {
            1.0
        }
    } else if input
        .try_parse(|candidate| candidate.expect_delim('/'))
        .is_ok()
    {
        parse_alpha_component(input)?
    } else {
        1.0
    };
    if first.is_finite() && second.is_finite() && third.is_finite() && alpha.is_finite() {
        Ok(CssColor::Srgb {
            red: rounded_rgb_channel(first),
            green: rounded_rgb_channel(second),
            blue: rounded_rgb_channel(third),
            alpha: alpha.clamp(0.0, 1.0),
        })
    } else {
        Err(location.new_custom_error(()))
    }
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

fn parse_rgb_component<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    match input.next()?.clone() {
        Token::Number { value, .. } => Ok(value),
        Token::Percentage { unit_value, .. } => Ok(unit_value * 255.0),
        _ => Err(location.new_custom_error(())),
    }
}

fn parse_alpha_component<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    match input.next()?.clone() {
        Token::Number { value, .. } => Ok(value),
        Token::Percentage { unit_value, .. } => Ok(unit_value),
        _ => Err(location.new_custom_error(())),
    }
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
    if words.len() == 1 {
        if let Some(value) = parse_single_display(&words[0]) {
            return Ok(value);
        }
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

fn parse_non_negative_number<'i>(input: &mut Parser<'i, '_>) -> CssResult<'i, f32> {
    let location = input.current_source_location();
    match input.next()?.clone() {
        Token::Number { value, .. } if value.is_finite() && value >= 0.0 => Ok(value),
        _ => Err(location.new_custom_error(())),
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
        Display, DisplayInside, DisplayOutside, LengthPercentage, MaxSize, Size,
        TypedPropertyValue, parse_typed_property,
    };

    fn parse(name: &str, css: &str) -> TypedPropertyValue {
        parse_typed_property(name, css)
            .expect("supported property")
            .expect("valid value")
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
}
