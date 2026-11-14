//! The declarative device layout (device handoff §3 and §7): pages of a grid of 3 rows and `cols`
//! columns, cells with a position, a span, an optional half-row slot, a widget kind and the
//! parameter it edits. Pure data and arithmetic: the frame draws it ([`super::frame`]), the
//! validator checks it in debug builds ([`validate`], called by [`super::model`]).
//!
//! A cell is `56 × 64`, the gap `4`, the margin `8` (`komponist_tokens::device::grid`), so column
//! `c` starts at `x = 8 + 60·c`, row `r` at `y = 8 + 68·r`. A half cell is `56 × 30` and sits in the
//! top (`y`) or bottom (`y + 34`) half of its row; two half cells share a row, a full cell never
//! shares with anything.

use std::collections::BTreeSet;
use std::fmt;

use egui::{Rect, pos2, vec2};
use komponist_model::params::{ParamContext, ParamId, ResolveError, context, resolve};
use komponist_tokens::{device, widget};

/// Rows of the grid.
pub const ROWS: u8 = 3;
/// Most pages a device has (`widget.pages.max_pages`).
pub const MAX_PAGES: usize = widget::pages::MAX_PAGES as usize;

/// The widget a cell shows (WP-83 and WP-84 build them in `komponist_ui::widgets::device`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WidgetKind {
    /// `knob`, 1 × 1.
    Knob,
    /// `knob_large`, 2 × 1.
    KnobLarge,
    /// `number`, 1 × ½.
    Number,
    /// `segment`, n × ½.
    Segment,
    /// `toggle_button`, n × ½.
    ToggleButton,
    /// `dropdown`, 2 × ½.
    Dropdown,
    /// `mini_fader`, 1 × 2 or 3.
    MiniFader,
    /// `meter_gr`, 1 × 3.
    MeterGr,
    /// `pad_grid`.
    PadGrid,
    /// `fm_algorithm`.
    FmAlgorithm,
    /// `graph` (WP-84), bound by [`CellParam::Graph`].
    Graph,
}

/// Which half of a row a half cell takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Half {
    /// `y`.
    Top,
    /// `y + 34`.
    Bottom,
}

/// The parameter a cell edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellParam {
    /// A fixed parameter.
    Id(ParamId),
    /// An id with context placeholders (`op{env_op}_attack`), resolved with the view's
    /// [`ParamContext`] ([`context::resolve`]).
    Template(String),
    /// A graph: a configuration and the parameters its slots edit.
    Graph(GraphBinding),
    /// A widget that edits no single parameter (a label, a meter).
    None,
}

/// The configurations of the graph (`komponist_ui::widgets::device::graph::configs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphConfig {
    /// `graph::envelope`.
    Envelope,
    /// `graph::eq` with this many bands.
    Eq {
        /// Bands.
        bands: u8,
    },
    /// `graph::filter`.
    Filter,
    /// `graph::transfer`.
    Transfer,
    /// `graph::waveform`.
    Waveform,
    /// `graph::xy`.
    Xy,
    /// `graph::lfo` with this many points.
    Lfo {
        /// Points.
        points: u8,
    },
}

/// What a graph cell shows and edits: the configuration, its title, and for every slot of the
/// configuration (by name: `attack`, `b1_freq`, …) the parameter id or template behind it. A slot
/// without a parameter keeps a fixed value and cannot be edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphBinding {
    /// The configuration.
    pub config: GraphConfig,
    /// The title (a template: `Op {env_op} · Amplitude`); empty keeps the configuration's own.
    pub title: String,
    /// Slot name → parameter id template.
    pub bind: Vec<(String, String)>,
    /// A context placeholder the selected handle sets: `band` for the EQ (handle `n` is band `n + 1`).
    pub select: Option<&'static str>,
}

impl GraphBinding {
    /// A binding of `config` without title, parameters or selection.
    pub fn new(config: GraphConfig) -> Self {
        Self {
            config,
            title: String::new(),
            bind: Vec::new(),
            select: None,
        }
    }

    /// With the title template `title`.
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Binds slot `slot` to the parameter (template) `param`.
    #[must_use]
    pub fn bind(mut self, slot: impl Into<String>, param: impl Into<String>) -> Self {
        self.bind.push((slot.into(), param.into()));
        self
    }

    /// The selected handle sets context placeholder `key`.
    #[must_use]
    pub fn selecting(mut self, key: &'static str) -> Self {
        self.select = Some(key);
        self
    }

    /// The parameter id behind slot `slot` under `context`: `None` for an unbound slot.
    pub fn resolved(
        &self,
        slot: &str,
        context: &ParamContext,
    ) -> Result<Option<ParamId>, ResolveError> {
        match self.bind.iter().find(|(s, _)| s == slot) {
            Some((_, template)) => resolve(template, context).map(Some),
            None => Ok(None),
        }
    }
}

/// One cell of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Left column.
    pub col: u8,
    /// Top row.
    pub row: u8,
    /// Columns spanned.
    pub cols: u8,
    /// Rows spanned (1 for a half cell).
    pub rows: u8,
    /// `Some` for a half cell.
    pub half: Option<Half>,
    /// The widget.
    pub widget: WidgetKind,
    /// The parameter.
    pub param: CellParam,
}

impl Cell {
    /// A knob in one cell.
    pub fn knob(col: u8, row: u8, param: ParamId) -> Cell {
        Cell {
            col,
            row,
            cols: 1,
            rows: 1,
            half: None,
            widget: WidgetKind::Knob,
            param: CellParam::Id(param),
        }
    }

    /// A widget over `cols × rows` full cells.
    pub fn span(
        col: u8,
        row: u8,
        cols: u8,
        rows: u8,
        widget: WidgetKind,
        param: CellParam,
    ) -> Cell {
        Cell {
            col,
            row,
            cols,
            rows,
            half: None,
            widget,
            param,
        }
    }

    /// A graph over `cols × rows` full cells.
    pub fn graph(col: u8, row: u8, cols: u8, rows: u8, binding: GraphBinding) -> Cell {
        Cell::span(
            col,
            row,
            cols,
            rows,
            WidgetKind::Graph,
            CellParam::Graph(binding),
        )
    }

    /// A widget in a half cell.
    pub fn half(
        col: u8,
        row: u8,
        cols: u8,
        half: Half,
        widget: WidgetKind,
        param: CellParam,
    ) -> Cell {
        Cell {
            col,
            row,
            cols,
            rows: 1,
            half: Some(half),
            widget,
            param,
        }
    }

    /// The cell's rectangle, relative to the top-left corner of the device body.
    pub fn rect(&self) -> Rect {
        let [cw, ch] = device::grid::CELL;
        let (gap, pad) = (device::grid::GAP, device::grid::PADDING);
        let x = pad + f32::from(self.col) * (cw + gap);
        let mut y = pad + f32::from(self.row) * (ch + gap);
        let (w, h) = (span(cw, self.cols), span(ch, self.rows));
        let h = match self.half {
            Some(half) => {
                if half == Half::Bottom {
                    y += device::grid::HALF_OFFSET_BOTTOM;
                }
                device::grid::HALF_CELL_H
            }
            None => h,
        };
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    /// The grid slots it occupies: (column, row, which part of the row).
    fn slots(&self) -> impl Iterator<Item = (u8, u8, Part)> + '_ {
        let part = match self.half {
            None => Part::Full,
            Some(Half::Top) => Part::Top,
            Some(Half::Bottom) => Part::Bottom,
        };
        (self.col..self.col.saturating_add(self.cols)).flat_map(move |c| {
            (self.row..self.row.saturating_add(self.rows)).map(move |r| (c, r, part))
        })
    }

    /// The parameter id the cell edits under `context`, `None` without one.
    pub fn resolved(&self, context: &ParamContext) -> Result<Option<ParamId>, ResolveError> {
        match &self.param {
            CellParam::Id(id) => Ok(Some(id.clone())),
            CellParam::Template(t) => resolve(t, context).map(Some),
            CellParam::Graph(_) | CellParam::None => Ok(None),
        }
    }
}

/// `n` cells with gaps between them: `n · cell + (n − 1) · gap` (`device.grid.span_w` / `span_h`).
pub fn span(cell: f32, n: u8) -> f32 {
    let n = f32::from(n.max(1));
    n * cell + (n - 1.0) * device::grid::GAP
}

/// The width of a device with `cols` columns: `16 + 60·cols − 4` (`device.grid.device_w`).
pub fn device_width(cols: u8) -> f32 {
    2.0 * device::grid::PADDING + span(device::grid::CELL[0], cols)
}

/// One page: a name for the page switch, its own column count, its cells.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    /// The name on the page switch.
    pub name: String,
    /// Columns (a device may have a different width per page).
    pub cols: u8,
    /// The cells.
    pub cells: Vec<Cell>,
    /// Columns after which a 1 px `line.1` divider runs in the gap (between function groups).
    pub dividers: Vec<u8>,
}

impl Page {
    /// The width of the device on this page.
    pub fn width(&self) -> f32 {
        device_width(self.cols)
    }
}

/// The layout of a device: 1 to [`MAX_PAGES`] pages.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceLayout {
    /// The pages; the page switch shows with two or more.
    pub pages: Vec<Page>,
}

impl DeviceLayout {
    /// One page without a name.
    pub fn single(page: Page) -> DeviceLayout {
        DeviceLayout { pages: vec![page] }
    }

    /// The knobs of `params`, row by row in `cols` columns (the auto-layout of a foreign plugin and
    /// of pisynth's macros until WP-86).
    pub fn knob_grid(params: impl IntoIterator<Item = ParamId>, cols: u8) -> DeviceLayout {
        let cols = cols.max(1);
        let cells: Vec<Cell> = params
            .into_iter()
            .take(usize::from(cols) * usize::from(ROWS))
            .enumerate()
            .map(|(i, id)| {
                Cell::knob(
                    (i % usize::from(cols)) as u8,
                    (i / usize::from(cols)) as u8,
                    id,
                )
            })
            .collect();
        DeviceLayout::single(Page {
            name: String::new(),
            cols,
            cells,
            dividers: Vec::new(),
        })
    }

    /// The page `index`, the last one if out of range.
    pub fn page(&self, index: usize) -> Option<&Page> {
        self.pages.get(index).or_else(|| self.pages.last())
    }
}

/// What is wrong with a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// No page at all.
    NoPages,
    /// More than [`MAX_PAGES`] pages.
    TooManyPages(usize),
    /// A page without columns.
    NoColumns {
        /// The page.
        page: usize,
    },
    /// A cell outside the grid or without size, or a half cell over several rows.
    BadCell {
        /// The page.
        page: usize,
        /// The cell.
        cell: usize,
    },
    /// Two cells share a grid slot (only two half cells of one cell may, top and bottom).
    Overlap {
        /// The page.
        page: usize,
        /// The earlier cell.
        a: usize,
        /// The later cell.
        b: usize,
    },
    /// A template that does not resolve.
    Unresolved {
        /// The page.
        page: usize,
        /// The cell.
        cell: usize,
        /// Why.
        error: ResolveError,
    },
    /// A parameter that the device does not have.
    UnknownParam {
        /// The page.
        page: usize,
        /// The cell.
        cell: usize,
        /// Its id.
        id: ParamId,
    },
    /// A divider outside the page.
    BadDivider {
        /// The page.
        page: usize,
    },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::NoPages => f.write_str("a layout needs a page"),
            LayoutError::TooManyPages(n) => write!(f, "{n} pages (at most {MAX_PAGES})"),
            LayoutError::NoColumns { page } => write!(f, "page {page} has no columns"),
            LayoutError::BadCell { page, cell } => {
                write!(
                    f,
                    "page {page}, cell {cell}: outside the grid or without size"
                )
            }
            LayoutError::Overlap { page, a, b } => {
                write!(f, "page {page}: cells {a} and {b} overlap")
            }
            LayoutError::Unresolved { page, cell, error } => {
                write!(f, "page {page}, cell {cell}: {error}")
            }
            LayoutError::UnknownParam { page, cell, id } => {
                write!(
                    f,
                    "page {page}, cell {cell}: unknown parameter `{}`",
                    id.as_str()
                )
            }
            LayoutError::BadDivider { page } => write!(f, "page {page}: divider outside the page"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Full,
    Top,
    Bottom,
}

/// Checks `layout`: at most [`MAX_PAGES`] pages, every cell inside the grid, no overlaps except
/// a top and a bottom half cell in one cell, every template resolvable with `context` and every
/// parameter known (`exists`). Returns every problem found.
pub fn validate(
    layout: &DeviceLayout,
    context: &ParamContext,
    exists: &dyn Fn(&ParamId) -> bool,
) -> Vec<LayoutError> {
    let mut errors = Vec::new();
    if layout.pages.is_empty() {
        errors.push(LayoutError::NoPages);
    }
    if layout.pages.len() > MAX_PAGES {
        errors.push(LayoutError::TooManyPages(layout.pages.len()));
    }
    for (p, page) in layout.pages.iter().enumerate() {
        if page.cols == 0 {
            errors.push(LayoutError::NoColumns { page: p });
        }
        if page.dividers.iter().any(|&d| d + 1 >= page.cols) {
            errors.push(LayoutError::BadDivider { page: p });
        }
        // Slot → the cell holding it (a Full slot blocks Top and Bottom too).
        let mut taken: Vec<((u8, u8, Part), usize)> = Vec::new();
        let mut seen_overlap = BTreeSet::new();
        for (c, cell) in page.cells.iter().enumerate() {
            let inside = cell.cols >= 1
                && cell.rows >= 1
                && cell.col.saturating_add(cell.cols) <= page.cols
                && cell.row.saturating_add(cell.rows) <= ROWS
                && (cell.half.is_none() || cell.rows == 1);
            if !inside {
                errors.push(LayoutError::BadCell { page: p, cell: c });
                continue;
            }
            for slot in cell.slots() {
                let clash = taken.iter().find(|((col, row, part), _)| {
                    (*col, *row) == (slot.0, slot.1)
                        && (*part == slot.2 || *part == Part::Full || slot.2 == Part::Full)
                });
                if let Some((_, a)) = clash
                    && seen_overlap.insert((*a, c))
                {
                    errors.push(LayoutError::Overlap {
                        page: p,
                        a: *a,
                        b: c,
                    });
                }
                taken.push((slot, c));
            }
            match cell.resolved(context) {
                Ok(Some(id)) if !exists(&id) => errors.push(LayoutError::UnknownParam {
                    page: p,
                    cell: c,
                    id,
                }),
                Err(error) => errors.push(LayoutError::Unresolved {
                    page: p,
                    cell: c,
                    error,
                }),
                _ => {}
            }
            if let CellParam::Graph(binding) = &cell.param {
                for (slot, _) in &binding.bind {
                    match binding.resolved(slot, context) {
                        Ok(Some(id)) if !exists(&id) => errors.push(LayoutError::UnknownParam {
                            page: p,
                            cell: c,
                            id,
                        }),
                        Err(error) => errors.push(LayoutError::Unresolved {
                            page: p,
                            cell: c,
                            error,
                        }),
                        _ => {}
                    }
                }
            }
        }
    }
    errors
}

/// The placeholder names of the layouts the editors know (re-exported for layout authors).
pub use context::{BAND, ENV_OP, ENV_VIEW, SEL};

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> ParamId {
        ParamId::new(s)
    }

    fn page(cols: u8, cells: Vec<Cell>) -> DeviceLayout {
        DeviceLayout::single(Page {
            name: "A".into(),
            cols,
            cells,
            dividers: vec![],
        })
    }

    fn all(_: &ParamId) -> bool {
        true
    }

    #[test]
    fn device_widths_follow_the_handoff() {
        assert_eq!(device_width(4), 252.0);
        assert_eq!(device_width(7), 432.0);
        assert_eq!(device_width(10), 612.0);
        assert_eq!(span(56.0, 2), 116.0);
    }

    #[test]
    fn cells_sit_on_the_grid() {
        let k = Cell::knob(2, 1, id("a"));
        assert_eq!(
            k.rect(),
            Rect::from_min_size(pos2(128.0, 76.0), vec2(56.0, 64.0))
        );
        let top = Cell::half(0, 2, 2, Half::Top, WidgetKind::Number, CellParam::None);
        let bottom = Cell::half(0, 2, 2, Half::Bottom, WidgetKind::Segment, CellParam::None);
        assert_eq!(top.rect().min.y, 144.0);
        assert_eq!(bottom.rect().min.y, 178.0);
        assert_eq!((top.rect().height(), top.rect().width()), (30.0, 116.0));
        let tall = Cell::span(0, 0, 1, 3, WidgetKind::MiniFader, CellParam::None);
        assert_eq!(tall.rect().height(), 200.0);
    }

    #[test]
    fn a_valid_layout_has_no_errors() {
        let l = page(
            4,
            vec![
                Cell::knob(0, 0, id("a")),
                Cell::knob(1, 0, id("b")),
                Cell::half(
                    2,
                    0,
                    1,
                    Half::Top,
                    WidgetKind::Number,
                    CellParam::Id(id("c")),
                ),
                Cell::half(
                    2,
                    0,
                    1,
                    Half::Bottom,
                    WidgetKind::ToggleButton,
                    CellParam::Id(id("d")),
                ),
                Cell::graph(
                    0,
                    1,
                    4,
                    2,
                    GraphBinding::new(GraphConfig::Transfer).bind("ratio", "a"),
                ),
            ],
        );
        assert_eq!(validate(&l, &ParamContext::new(), &all), vec![]);
    }

    #[test]
    fn overlaps_are_found_but_two_half_cells_may_share() {
        let l = page(
            4,
            vec![
                Cell::knob(0, 0, id("a")),
                Cell::span(0, 0, 2, 1, WidgetKind::KnobLarge, CellParam::Id(id("b"))),
                Cell::half(3, 0, 1, Half::Top, WidgetKind::Number, CellParam::None),
                Cell::half(3, 0, 1, Half::Top, WidgetKind::Number, CellParam::None),
                Cell::half(2, 1, 1, Half::Top, WidgetKind::Number, CellParam::None),
                Cell::knob(2, 1, id("c")),
            ],
        );
        let errors = validate(&l, &ParamContext::new(), &all);
        assert_eq!(
            errors,
            vec![
                LayoutError::Overlap {
                    page: 0,
                    a: 0,
                    b: 1
                },
                LayoutError::Overlap {
                    page: 0,
                    a: 2,
                    b: 3
                },
                LayoutError::Overlap {
                    page: 0,
                    a: 4,
                    b: 5
                },
            ]
        );
    }

    #[test]
    fn cells_outside_the_grid_and_too_many_pages_are_errors() {
        let l = page(
            2,
            vec![
                Cell::knob(2, 0, id("a")),
                Cell::knob(0, 3, id("b")),
                Cell::span(0, 0, 1, 0, WidgetKind::Knob, CellParam::None),
                Cell::half(0, 0, 1, Half::Top, WidgetKind::Number, CellParam::None).with_rows(2),
            ],
        );
        let errors = validate(&l, &ParamContext::new(), &all);
        assert_eq!(errors.len(), 4);
        assert!(
            errors
                .iter()
                .all(|e| matches!(e, LayoutError::BadCell { .. }))
        );
        let many = DeviceLayout {
            pages: (0..5)
                .map(|i| Page {
                    name: i.to_string(),
                    cols: 1,
                    ..Page::default()
                })
                .collect(),
        };
        assert_eq!(
            validate(&many, &ParamContext::new(), &all),
            vec![LayoutError::TooManyPages(5)]
        );
        assert_eq!(
            validate(&DeviceLayout::default(), &ParamContext::new(), &all),
            vec![LayoutError::NoPages]
        );
    }

    #[test]
    fn parameters_must_exist_and_templates_must_resolve() {
        let l = page(
            4,
            vec![
                Cell::knob(0, 0, id("known")),
                Cell::knob(1, 0, id("missing")),
                Cell::span(
                    2,
                    0,
                    1,
                    1,
                    WidgetKind::Knob,
                    CellParam::Template("op{env_op}_attack".into()),
                ),
            ],
        );
        let exists = |p: &ParamId| p.as_str() != "missing" && p.as_str() != "op9_attack";
        let ctx = ParamContext::new().with(ENV_OP, 2);
        assert_eq!(
            validate(&l, &ctx, &exists),
            vec![LayoutError::UnknownParam {
                page: 0,
                cell: 1,
                id: id("missing")
            }]
        );
        // Placeholder without a value, and a resolved id the device lacks.
        let errors = validate(&l, &ParamContext::new(), &exists);
        assert!(matches!(
            errors.last(),
            Some(LayoutError::Unresolved {
                cell: 2,
                error: ResolveError::Missing(_),
                ..
            })
        ));
        let nine = ParamContext::new().with(ENV_OP, 9);
        assert!(matches!(
            validate(&l, &nine, &exists).last(),
            Some(LayoutError::UnknownParam { cell: 2, .. })
        ));
    }

    #[test]
    fn the_knob_grid_is_four_columns_of_up_to_twelve() {
        let ids = (0..20).map(|i| id(&format!("p{i}")));
        let l = DeviceLayout::knob_grid(ids, 4);
        let page = &l.pages[0];
        assert_eq!((page.cols, page.cells.len()), (4, 12));
        assert_eq!((page.cells[5].col, page.cells[5].row), (1, 1));
        assert_eq!(validate(&l, &ParamContext::new(), &all), vec![]);
        let two = DeviceLayout::knob_grid([id("a"), id("b")], 4);
        assert_eq!(
            two.pages[0].width(),
            252.0,
            "a short row keeps the frame's width"
        );
    }

    impl Cell {
        fn with_rows(mut self, rows: u8) -> Cell {
            self.rows = rows;
            self
        }
    }

    #[test]
    fn a_graph_binding_is_checked_slot_by_slot() {
        let binding = GraphBinding::new(GraphConfig::Envelope)
            .bind("attack", "op{env_op}_attack")
            .bind("decay", "op{env_op}_missing")
            .bind("release", "{nothing}_r");
        let l = page(6, vec![Cell::graph(0, 0, 6, 2, binding)]);
        let exists = |p: &ParamId| p.as_str() != "op2_missing";
        let ctx = ParamContext::new().with(ENV_OP, 2);
        let errors = validate(&l, &ctx, &exists);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            matches!(&errors[0], LayoutError::UnknownParam { id, .. } if id.as_str() == "op2_missing")
        );
        assert!(matches!(&errors[1], LayoutError::Unresolved { .. }));
        // A slot without a parameter is fine and resolves to nothing.
        let b = GraphBinding::new(GraphConfig::Envelope).bind("attack", "a");
        assert_eq!(b.resolved("curve", &ctx), Ok(None));
        assert_eq!(b.resolved("attack", &ctx), Ok(Some(id("a"))));
    }
}
