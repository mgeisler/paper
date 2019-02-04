//! A terminal-based editor with goals to maximize simplicity and efficiency.
//!
//! This project is very much in an alpha state.
//!
//! Its features include:
//! - Modal editing (keys implement different functionality depending on the current mode).
//! - Extensive but relatively simple filter grammar that allows user to select any text.
//!
//! Future items on the Roadmap:
//! - Add more filter grammar.
//! - Implement suggestions for commands to improve user experience.
//! - Support Language Server Protocol.
//!
//! # Usage
//!
//! To use paper, install and run the binary. If you are developing a rust crate that runs paper,
//! then create and run an instance by calling the following:

// Lint checks currently not defined: missing_doc_code_examples, variant_size_differences
#![warn(
    rust_2018_idioms,
    future_incompatible,
    unused,
    box_pointers,
    macro_use_extern_crate,
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    single_use_lifetimes,
    trivial_casts,
    trivial_numeric_casts,
    unreachable_pub,
    unused_import_braces,
    unused_lifetimes,
    unused_qualifications,
    unused_results,
    clippy::nursery,
    clippy::pedantic,
    //clippy::restriction,
    clippy::result_unwrap_used,
)]
#![allow(clippy::suspicious_op_assign_impl, clippy::suspicious_arithmetic_impl)] // These lints are not always correct; issues should be detected by tests.
#![doc(html_root_url = "https://docs.rs/paper/0.2.0")]

mod engine;
mod ui;

use crate::engine::{Controller, Notice};
use crate::ui::{Address, Change, Color, Index, Edit, Length, Region, UserInterface, END, IndexType};
use rec::ChCls::{Any, Digit, End, Sign};
use rec::{Element, tkn, some, Pattern};
use std::borrow::Borrow;
use std::cmp::{self, Ordering};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter, Result as FmtResult};
use std::fs;
use std::io;
use std::iter;
use std::ops::{Add, AddAssign, Shr, ShrAssign, Sub};
use try_from::{TryFrom, TryInto, TryFromIntError};

const NEGATIVE_ONE: IndexType = -1;

/// The paper application.
// In general, Paper methods should contain as little logic as possible. Instead all logic should
// be included in Operations.
#[derive(Debug, Default)]
pub struct Paper {
    /// User interface of the application.
    ui: UserInterface,
    controller: Controller,
    /// Data of the file being edited.
    view: View,
    /// Characters being edited to be analyzed by the application.
    sketch: String,
    /// [`Section`]s of the view that match the current filter.
    ///
    /// [`Section`]: .struct.Section.html
    signals: Vec<Section>,
    noises: Vec<Section>,
    marks: Vec<Mark>,
    filters: PaperFilters,
    sketch_additions: String,
}

impl Paper {
    /// Creates a new paper application.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs the application.
    #[inline]
    pub fn run(&mut self) -> Result<(), engine::Failure> {
        self.ui.init()?;
        let operations = engine::Operations::default();

        'main: loop {
            for opcode in self.controller.process_input(self.ui.receive_input()) {
                match operations.operate(self, opcode)? {
                    Some(Notice::Quit) => break 'main,
                    Some(Notice::Flash) => {
                        self.ui.flash()?;
                    }
                    None => {}
                }
            }
        }

        self.ui.close()?;
        Ok(())
    }

    /// Displays the view on the user interface.
    fn display_view(&self) -> Result<(), ui::Fault> {
        for edit in self.view.redraw_edits().take(self.ui.grid_height().unwrap()) {
            self.ui.apply(edit)?;
        }

        Ok(())
    }

    fn change_view(&mut self, path: &str) -> Result<(), TryFromIntError> {
        self.view = View::with_file(String::from(path))?;
        self.noises.clear();

        for line in 1..=self.view.line_count {
            if let Some(noise) = LineNumber::new(line).map(Section::line) {
                self.noises.push(noise);
            }
        }

        Ok(())
    }

    fn save_view(&self) {
        self.view.put();
    }

    fn reduce_noise(&mut self) {
        self.noises = self.signals.clone();
    }

    fn filter_signals(&mut self, feature: &str) -> Result<(), TryFromIntError> {
        self.signals = self.noises.clone();

        if let Some(id) = feature.chars().nth(0) {
            for filter in self.filters.iter() {
                if id == filter.id() {
                    return filter.extract(feature, &mut self.signals, &self.view);
                }
            }
        }

        Ok(())
    }

    fn sketch(&self) -> &String {
        &self.sketch
    }

    fn sketch_mut(&mut self) -> &mut String {
        &mut self.sketch
    }

    fn draw_sketch(&self) -> Result<(), engine::Failure> {
        Ok(self.ui
            .apply(Edit::new(Region::row(0)?, Change::Row(self.sketch.clone())))?)
    }

    fn clear_background(&self) -> Result<(), engine::Failure> {
        for row in 0..self.ui.grid_height().unwrap() {
            self.format_region(Region::row(row)?, Color::Default)?;
        }

        Ok(())
    }

    fn set_marks(&mut self, edge: Edge) {
        self.marks.clear();

        for signal in &self.signals {
            let mut place = signal.start;

            if edge == Edge::End {
                place.column += Index::try_from(signal.length).unwrap_or_else(|_| self.view.line_length(signal.start).unwrap_or_default())
            }
            
            let mut pointer = Pointer(match place.line.row() {
                0 => Some(Index::from(0)),
                row => self.view.data.match_indices(ui::ENTER).nth(row - 1).and_then(|x| Index::try_from(x.0 + 1).ok()),
            });
            pointer += place.column;
            self.marks.push(Mark {
                place,
                pointer,
            });
        }
    }

    fn scroll(&mut self, movement: IndexType) -> Result<(), ui::Fault> {
        self.view.scroll(movement);
        self.display_view()
    }

    fn draw_filter_backgrounds(&self) -> Result<(), ui::Fault> {
        for noise in &self.noises {
            self.format_section(noise, Color::Blue)?;
        }

        for signal in &self.signals {
            self.format_section(signal, Color::Red)?;
        }

        Ok(())
    }

    /// Sets the [`Color`] of a [`Section`].
    fn format_section(&self, section: &Section, color: Color) -> Result<(), ui::Fault> {
        // It is okay for region_at() to return None; this just means section is not displayed.
        if let Some(region) = self.view.region_at(section) {
            self.format_region(region, color)?;
        }

        Ok(())
    }

    fn format_region(&self, region: Region, color: Color) -> Result<(), ui::Fault> {
        self.ui.apply(Edit::new(region, Change::Format(color)))
    }

    fn update_view(&mut self, c: char) -> Result<(), engine::Failure> {
        let mut adjustment = Adjustment::default();

        for mark in &mut self.marks {
            if let Some(new_adjustment) = Adjustment::create(c, mark.place, &self.view) {
                adjustment += new_adjustment;

                if adjustment.change != Change::Clear {
                    if let Some(region) = self.view.region_at(&mark.place) {
                        self.ui
                            .apply(Edit::new(region, adjustment.change.clone()))?;
                    }
                }

                mark.adjust(&adjustment);
                self.view.add(mark, c)?;
            }
        }

        if adjustment.change == Change::Clear {
            self.view.clean()?;
            self.display_view()?;
        }

        Ok(())
    }

    fn change_mode(&mut self, mode: engine::Mode) {
        self.controller.set_mode(mode);
    }

    /// Returns the height used for scrolling.
    fn scroll_height(&self) -> usize {
        self.ui.grid_height().unwrap() / 4
    }
}

#[derive(Debug, Default)]
struct PaperFilters {
    line: LineFilter,
    pattern: PatternFilter,
}

impl PaperFilters {
    fn iter(&self) -> PaperFiltersIter<'_> {
        PaperFiltersIter {
            index: 0,
            filters: self,
        }
    }
}

struct PaperFiltersIter<'a> {
    index: usize,
    filters: &'a PaperFilters,
}

impl<'a> Iterator for PaperFiltersIter<'a> {
    type Item = &'a dyn Filter;

    fn next(&mut self) -> Option<Self::Item> {
        self.index += 1;

        match self.index {
            1 => Some(&self.filters.line),
            2 => Some(&self.filters.pattern),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct View {
    data: String,
    first_line: LineNumber,
    margin_width: usize,
    line_count: usize,
    path: String,
}

impl View {
    fn with_file(path: String) -> Result<Self, TryFromIntError> {
        let mut view = Self {
            data: fs::read_to_string(path.as_str()).unwrap().replace('\r', ""),
            path,
            ..Self::default()
        };

        view.clean()?;
        Ok(view)
    }

    fn add(&mut self, mark: &Mark, c: char) -> Result<(), TryFromIntError> {
        if let Some(index) = mark.pointer.0 {
            let data_index = usize::try_from(index)?;

            if let ui::BACKSPACE = c {
                // For now, do not care to check what is removed. But this may become important for
                // multi-byte characters.
                match self.data.remove(data_index) {
                    _ => {}
                }
            } else {
                self.data.insert(data_index - 1, c);
            }
        }

        Ok(())
    }

    fn address_at(&self, place: Place) -> Option<Address> {
        match Index::try_from(place.line - self.first_line) {
            Ok(row) => IndexType::try_from(self.margin_width).ok().map(|origin| Address::new(row, place.column + origin)),
            _ => None,
        }
    }

    fn region_at<T: RegionWrapper>(&self, region_wrapper: &T) -> Option<Region> {
        self.address_at(region_wrapper.start()).map(|address| Region::new(address, region_wrapper.length()))
    }

    fn redraw_edits(&self) -> impl Iterator<Item = Edit> + '_ {
        // Clear the screen, then add each row.
        iter::once(Edit::new(Region::default(), Change::Clear)).chain(
            self.lines()
                .skip(self.first_line.row())
                .enumerate()
                .flat_map(move |x| {
                    Region::row(x.0).map(|region|
                        Edit::new(
                            region,
                            Change::Row(format!(
                                "{:>width$} {}",
                                x.0 + self.first_line.row() + 1,
                                x.1,
                                width = self.margin_width - 1
                            )),
                        )
                    ).into_iter()
                })
        )
    }

    fn lines(&self) -> std::str::Lines<'_> {
        self.data.lines()
    }

    fn line(&self, line_number: LineNumber) -> Option<&str> {
        self.lines().nth(line_number.row())
    }

    fn clean(&mut self) -> Result<(), TryFromIntError> {
        self.line_count = self.lines().count();
        self.update_margin_width()
    }

    #[allow(clippy::cast_sign_loss)] // self.line_count >= 0, thus log10().ceil() >= 0.0
    #[allow(clippy::cast_possible_truncation)] // usize.log10().ceil() < usize.max_value()
    fn update_margin_width(&mut self) -> Result<(), TryFromIntError> {
        self.margin_width = (f64::from(u32::try_from(self.line_count + 1)?)).log10().ceil() as usize + 1;

        Ok(())
    }

    fn scroll(&mut self, movement: IndexType) {
        self.first_line = cmp::min(
            self.first_line + movement,
            LineNumber::new(self.line_count).unwrap_or_default(),
        );
    }

    fn line_length(&self, place: Place) -> Option<Index> {
        self.line(place.line).and_then(|x| Index::try_from(x.len()).ok())
    }

    fn put(&self) -> io::Result<()>  {
        fs::write(&self.path, &self.data)
    }
}

#[derive(Clone, Debug, Default)]
struct Adjustment {
    shift: IndexType,
    line_change: IndexType,
    indexes_changed: HashMap<LineNumber, IndexType>,
    change: Change,
}

impl Adjustment {
    /// Creates a new `Adjustment`.
    fn new(line: LineNumber, shift: IndexType, index_change: IndexType, change: Change) -> Self {
        let line_change = if change == Change::Clear { shift } else { 0 };

        Self {
            shift,
            line_change,
            indexes_changed: [(line + line_change, index_change)]
                .iter()
                .cloned()
                .collect(),
            change,
        }
    }

    /// Creates an `Adjustment` based on the given context.
    fn create(c: char, place: Place, view: &View) -> Option<Self> {
        match c {
            ui::BACKSPACE => {
                if place.column == 0 {
                    view.line_length(place)
                        .map(|x| Self::new(place.line, NEGATIVE_ONE, IndexType::from(x), Change::Clear))
                } else {
                    Some(Self::new(place.line, NEGATIVE_ONE, NEGATIVE_ONE, Change::Backspace))
                }
            }
            ui::ENTER => Some(Self::new(place.line, 1, -place.column, Change::Clear)),
            _ => Some(Self::new(place.line, 1, 1, Change::Insert(c))),
        }
    }
}

impl AddAssign for Adjustment {
    fn add_assign(&mut self, other: Self) {
        self.shift += other.shift;
        self.line_change += other.line_change;

        for (line, change) in other.indexes_changed {
            *self.indexes_changed.entry(line).or_default() += change;
        }

        if self.change != Change::Clear {
            self.change = other.change
        }
    }
}

/// Indicates a specific Place of a given Section.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub enum Edge {
    /// Indicates the first Place of the Section.
    Start,
    /// Indicates the last Place of the Section.
    End,
}

impl Default for Edge {
    #[inline]
    fn default() -> Self {
        Edge::Start
    }
}

impl Display for Edge {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Edge::Start => write!(f, "Starting edge"),
            Edge::End => write!(f, "Ending edge"),
        }
    }
}

/// An address and its respective pointer in a view.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
struct Mark {
    /// Pointer in view that corresponds with mark.
    pointer: Pointer,
    /// Place of mark.
    place: Place,
}

impl Mark {
    /// Moves `Mark` as specified by the given [`Adjustment`].
    fn adjust(&mut self, adjustment: &Adjustment) {
        self.pointer += adjustment.shift;
        self.place.line = self.place.line + adjustment.line_change;

        for (&line, &change) in &adjustment.indexes_changed {
            if line == self.place.line {
                self.place >>= change;
            }
        }
    }
}

impl Display for Mark {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "{}{}", self.place, self.pointer)
    }
}

/// Signifies an index of a character within [`View`].
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Debug)]
struct Pointer(Option<Index>);

impl PartialEq<IndexType> for Pointer {
    fn eq(&self, other: &IndexType) -> bool {
        self.0.map_or(false, |x| x == *other)
    }
}

impl PartialOrd<IndexType> for Pointer {
    fn partial_cmp(&self, other: &IndexType) -> Option<Ordering> {
        self.0.and_then(|x| x.partial_cmp(other))
    }
}

impl<T: Borrow<IndexType>> AddAssign<T> for Pointer {
    fn add_assign(&mut self, other: T) {
        self.0 = self.0.map(|x| x + *other.borrow());
    }
}

impl Default for Pointer {
    fn default() -> Self {
        Pointer(Some(Index::from(0)))
    }
}

impl Display for Pointer {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(
            f,
            "[{}]",
            match self.0 {
                None => String::from("None"),
                Some(i) => format!("{}", i),
            }
        )
    }
}

impl PartialEq<Pointer> for IndexType {
    #[inline]
    fn eq(&self, other: &Pointer) -> bool {
        other == self
    }
}

impl PartialOrd<Pointer> for IndexType {
    #[inline]
    fn partial_cmp(&self, other: &Pointer) -> Option<Ordering> {
        other.partial_cmp(self).map(|x| x.reverse())
    }
}

/// Signifies a type that can be converted to a [`Region`].
trait RegionWrapper {
    /// Returns the starting `Place`.
    fn start(&self) -> Place;
    /// Returns the [`Length`].
    fn length(&self) -> Length;
}

/// Signifies adjacent [`Place`]s.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default)]
pub struct Section {
    /// The [`Place`] at which `Section` starts.
    start: Place,
    /// The [`Length`] of `Section`.
    length: Length,
}

impl Section {
    /// Creates a new `Section` that signifies an entire line.
    #[inline]
    fn line(line: LineNumber) -> Self {
        Self {
            start: Place { line, column: Index::from(0) },
            length: END,
        }
    }
}

impl RegionWrapper for Section {
    fn start(&self) -> Place {
        self.start
    }

    fn length(&self) -> Length {
        self.length
    }
}

impl Display for Section {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "{}->{}", self.start, self.length)
    }
}

/// Signifies the location of a character within a view.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
pub struct Place {
    /// The [`LineNumber`] of `Place`.
    line: LineNumber,
    /// The [`Index`] of the column of `Place`.
    column: Index,
}

impl RegionWrapper for Place {
    fn start(&self) -> Place {
        *self
    }

    fn length(&self) -> Length {
        Length::from(1)
    }
}

impl Shr<IndexType> for Place {
    type Output = Self;

    #[inline]
    fn shr(self, rhs: IndexType) -> Self {
        let mut new_place = self;
        new_place >>= rhs;
        new_place
    }
}

impl ShrAssign<IndexType> for Place {
    #[inline]
    fn shr_assign(&mut self, rhs: IndexType) {
        self.column += rhs;
    }
}

impl Display for Place {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "ln {}, idx {}", self.line, self.column)
    }
}

/// The type of the value stored in [`LineNumber`].
type LineNumberType = u32;

/// Signifies a line number.
#[derive(Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Hash, Debug)]
struct LineNumber(LineNumberType);

impl LineNumber {
    /// Creates a new `LineNumber`.
    fn new(value: usize) -> Option<Self> {
        if value == 0 {
            None
        } else {
            value.try_into().ok().map(LineNumber)
        }
    }

    /// Converts `LineNumber` to its row index - assuming line number `1` as at row `0`.
    #[allow(clippy::integer_arithmetic)] // self.0 >= 0
    fn row(self) -> usize {
        (self.0 - 1) as usize
    }
}

impl Add<IndexType> for LineNumber {
    type Output = Self;

    fn add(self, other: IndexType) -> Self::Output {
        #[allow(clippy::integer_arithmetic)] // i64::min_value() <= u32 + i32 <= i64::max_value()
        match LineNumberType::try_from(i64::from(self.0) + i64::from(other)) {
            Err(TryFromIntError::Underflow) => Self::default(),
            Err(TryFromIntError::Overflow) => Self(LineNumberType::max_value()),
            Ok(sum) => LineNumber(sum),
        }
    }
}

impl Sub for LineNumber {
    type Output = i64;

    #[allow(clippy::integer_arithmetic)] // self.0 and other.0 <= u32::MAX
    fn sub(self, other: Self) -> Self::Output {
        i64::from(self.0) - i64::from(other.0)
    }
}

impl Display for LineNumber {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "{}", self.0)
    }
}

impl Default for LineNumber {
    #[inline]
    fn default() -> Self {
        LineNumber(1)
    }
}

impl std::str::FromStr for LineNumber {
    type Err = ParseLineNumberError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.parse::<usize>()?).ok_or(ParseLineNumberError::InvalidValue)
    }
}

/// Signifies an error that occurs while parsing a [`LineNumber`] from a [`String`].
#[derive(Debug)]
enum ParseLineNumberError {
    /// The parsed number was not a valid line number.
    InvalidValue,
    /// There was an issue parsing the given string to an integer.
    ParseInt(std::num::ParseIntError),
}

impl std::error::Error for ParseLineNumberError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            ParseLineNumberError::InvalidValue => None,
            ParseLineNumberError::ParseInt(ref err) => Some(err),
        }
    }
}

impl Display for ParseLineNumberError {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match *self {
            ParseLineNumberError::InvalidValue => write!(f, "Invalid line number provided."),
            ParseLineNumberError::ParseInt(ref err) => write!(f, "{}", err),
        }
    }
}

impl From<std::num::ParseIntError> for ParseLineNumberError {
    fn from(error: std::num::ParseIntError) -> Self {
        ParseLineNumberError::ParseInt(error)
    }
}

/// Used for modifying [`Section`]s to match a feature.
trait Filter: Debug {
    /// Returns the identifying character of the `Filter`.
    fn id(&self) -> char;
    /// Modifies `sections` such that it matches the given feature.
    fn extract(&self, feature: &str, sections: &mut Vec<Section>, view: &View) -> Result<(), TryFromIntError>;
}

/// The [`Filter`] used to match a line.
#[derive(Debug)]
struct LineFilter {
    /// The [`Pattern`] used to match one or more [`LineNumber`]s.
    pattern: Pattern,
}

impl Default for LineFilter {
    fn default() -> Self {
        Self {
            pattern: Pattern::define(
                "#" + ((tkn!(some(Digit) => "line") + End)
                    | (tkn!(some(Digit) => "start") + "." + tkn!(some(Digit) => "end"))
                    | (tkn!(some(Digit) => "origin")
                        + tkn!(Sign + some(Digit) => "movement"))),
            ),
        }
    }
}

impl Filter for LineFilter {
    fn id(&self) -> char {
        '#'
    }

    fn extract(&self, feature: &str, sections: &mut Vec<Section>, _view: &View) -> Result<(), TryFromIntError> {
        let tokens = self.pattern.tokenize(feature);

        if let Ok(line) = tokens.parse::<LineNumber>("line") {
            sections.retain(|&x| x.start.line == line);
        } else if let (Ok(start), Ok(end)) = (tokens.parse::<LineNumber>("start"), tokens.parse::<LineNumber>("end")) {
            let top = cmp::min(start, end);
            let bottom = cmp::max(start, end);

            sections.retain(|&x| {
                let row = x.start.line;
                row >= top && row <= bottom
            })
        } else if let (Ok(origin), Ok(movement)) = (tokens.parse::<LineNumber>("origin"), tokens.parse::<IndexType>("movement")) {
            let end = origin + movement;
            let top = cmp::min(origin, end);
            let bottom = cmp::max(origin, end);

            sections.retain(|&x| {
                let row = x.start.line;
                row >= top && row <= bottom
            })
        }

        Ok(())
    }
}

/// A [`Filter`] that extracts matches of a [`Pattern`].
#[derive(Debug)]
struct PatternFilter {
    /// The [`Pattern`] used to match patterns.
    pattern: Pattern,
}

impl Default for PatternFilter {
    fn default() -> Self {
        Self {
            pattern: Pattern::define("/" + tkn!(some(Any) => "pattern")),
        }
    }
}

impl Filter for PatternFilter {
    fn id(&self) -> char {
        '/'
    }

    fn extract(&self, feature: &str, sections: &mut Vec<Section>, view: &View) -> Result<(), TryFromIntError> {
        if let Some(user_pattern) = self.pattern.tokenize(feature).get("pattern") {
            if let Ok(search_pattern) = Pattern::load(user_pattern) {
                let target_sections = sections.clone();
                sections.clear();

                for target_section in target_sections {
                    let start = usize::try_from(target_section.start.column)?;

                    if let Some(target) = view.line(target_section.start.line).map(|x| {
                        x.chars()
                            .skip(start)
                            .collect::<String>()
                    }) {
                        for location in search_pattern.locate_iter(&target) {
                            sections.push(Section {
                                start: target_section.start >> IndexType::try_from(location.start())?,
                                length: Length::try_from(location.length())?,
                            });
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
