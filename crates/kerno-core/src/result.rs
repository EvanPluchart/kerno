//! Result sets: column metadata and compact row storage.
//!
//! Drivers deliver every value in the server's own text form (or raw bytes
//! for binary columns). Rows are packed into [`RowBatch`] arenas: one byte
//! buffer and one offset per cell, never an allocation per cell, each
//! allocated at its exact size once the batch is finished. A
//! [`ResultStore`] strings batches together and addresses rows globally.

use std::sync::Arc;

/// Broad family of a column's type: decides rendering (alignment, NULL and
/// boolean styling, viewers), client-side sort order and literal quoting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeKind {
    /// Whole numbers of any width, signed or unsigned.
    Integer,
    /// Binary floating point.
    Float,
    /// Exact decimals (`numeric`, `decimal`, `money`).
    Decimal,
    /// True/false.
    Boolean,
    /// Character data.
    Text,
    /// Raw bytes; cells hold the bytes themselves, not an encoding of them.
    Binary,
    /// Calendar date.
    Date,
    /// Time of day, with or without zone.
    Time,
    /// Date and time without zone.
    DateTime,
    /// Date and time with zone.
    DateTimeTz,
    /// Time span.
    Interval,
    /// UUID.
    Uuid,
    /// JSON document.
    Json,
    /// Array of another type.
    Array,
    /// Enumerated type or set.
    Enum,
    /// Bit string.
    Bit,
    /// Anything else, shown as the server prints it.
    Other,
}

impl TypeKind {
    /// Numbers are right-aligned in the grid and sorted numerically.
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Integer | Self::Float | Self::Decimal)
    }
}

/// One column of a result set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// Column label as returned by the server.
    pub name: String,
    /// Type name as the engine spells it (`int4`, `varchar`, `BIGINT UNSIGNED`).
    pub type_name: String,
    /// Family of the type.
    pub kind: TypeKind,
    /// The table column the server says this column shows, when it shows
    /// one: an expression or a literal has none.
    pub origin: Option<ColumnOrigin>,
}

/// Where a column of a result comes from, as the server describes it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ColumnOrigin {
    /// PostgreSQL's row description: the table's OID and the column's
    /// attribute number, named by a catalog lookup in the same database.
    Oid { table: u32, column: i16 },
    /// MySQL and MariaDB: the database, the table and the column under
    /// their own names, whatever aliases the statement gave them.
    Named {
        schema: String,
        table: String,
        column: String,
    },
}

impl Column {
    /// A column with the given name, type name and kind, of no known table.
    pub fn new(name: impl Into<String>, type_name: impl Into<String>, kind: TypeKind) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            kind,
            origin: None,
        }
    }

    /// The same column, showing a table column.
    pub fn with_origin(mut self, origin: Option<ColumnOrigin>) -> Self {
        self.origin = origin;
        self
    }
}

/// A run of rows stored row-major in one arena.
///
/// `ends[i]` is the end offset in `data` of cell `i` (row-major); a cell
/// starts where the previous one ends. NULL is a bit in `nulls`, so it stays
/// distinct from an empty value. The three buffers are exactly as long as
/// what they hold: a finished batch has no spare capacity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowBatch {
    columns: usize,
    ends: Box<[u32]>,
    nulls: Box<[u64]>,
    data: Box<[u8]>,
}

impl RowBatch {
    /// Number of rows in the batch.
    pub fn row_count(&self) -> usize {
        self.ends.len().checked_div(self.columns).unwrap_or(0)
    }

    /// Number of columns per row.
    pub fn column_count(&self) -> usize {
        self.columns
    }

    /// The bytes of a cell, or `None` for NULL.
    ///
    /// # Panics
    /// Panics if `row` or `column` is out of range.
    pub fn cell(&self, row: usize, column: usize) -> Option<&[u8]> {
        assert!(column < self.columns, "column {column} out of range");
        let index = row * self.columns + column;
        if self.nulls[index / 64] & (1 << (index % 64)) != 0 {
            return None;
        }
        let end = self.ends[index] as usize;
        let start = if index == 0 {
            0
        } else {
            self.ends[index - 1] as usize
        };
        Some(&self.data[start..end])
    }

    /// A cell as text, or `None` for NULL. Invalid UTF-8 (only possible in
    /// binary columns) yields an empty string; read those with [`Self::cell`].
    pub fn text(&self, row: usize, column: usize) -> Option<&str> {
        self.cell(row, column)
            .map(|bytes| std::str::from_utf8(bytes).unwrap_or_default())
    }

    /// Heap bytes held by the batch.
    pub fn memory_bytes(&self) -> usize {
        size_of_val(&*self.ends) + size_of_val(&*self.nulls) + self.data.len()
    }
}

/// Builds a [`RowBatch`] cell by cell. Cells are appended in row-major order;
/// a row is complete after `columns` cells.
///
/// Cells go into scratch buffers that the builder keeps from one batch to
/// the next, so they stop growing after the first batch; [`Self::finish`]
/// copies them into a batch of exactly their size.
#[derive(Debug)]
pub struct BatchBuilder {
    columns: usize,
    ends: Vec<u32>,
    nulls: Vec<u64>,
    data: Vec<u8>,
}

/// A batch cannot address more than this many data bytes (`u32` offsets).
/// Builders report [`BatchBuilder::is_full`] well before the limit.
const MAX_BATCH_BYTES: usize = 64 * 1024 * 1024;

/// A scratch buffer that grew past this (rows of large values) is released
/// when its batch is finished rather than kept for the next one: a paused
/// statement would otherwise hold it for as long as it stays open.
const SCRATCH_KEPT_BYTES: usize = 4 * 1024 * 1024;

/// Copies `scratch` into an allocation of its exact size and empties it.
fn take<T: Copy>(scratch: &mut Vec<T>) -> Box<[T]> {
    let exact = Box::from(scratch.as_slice());
    if scratch.capacity() * size_of::<T>() > SCRATCH_KEPT_BYTES {
        *scratch = Vec::new();
    } else {
        scratch.clear();
    }
    exact
}

impl BatchBuilder {
    /// A builder for rows of `columns` cells.
    pub fn new(columns: usize) -> Self {
        Self {
            columns,
            ends: Vec::new(),
            nulls: Vec::new(),
            data: Vec::new(),
        }
    }

    fn push(&mut self, value: Option<&[u8]>) {
        let index = self.ends.len();
        if index.is_multiple_of(64) {
            self.nulls.push(0);
        }
        match value {
            Some(bytes) => self.data.extend_from_slice(bytes),
            None => self.nulls[index / 64] |= 1 << (index % 64),
        }
        // A single cell larger than 4 GiB cannot be addressed; no protocol
        // Kerno speaks can deliver one (PostgreSQL and MySQL cap a value at
        // 1 GiB), so saturating keeps the batch consistent instead of panicking.
        self.ends
            .push(u32::try_from(self.data.len()).unwrap_or(u32::MAX));
    }

    /// Appends a NULL cell.
    pub fn push_null(&mut self) {
        self.push(None);
    }

    /// Appends a text cell.
    pub fn push_text(&mut self, value: &str) {
        self.push(Some(value.as_bytes()));
    }

    /// Appends a cell of raw bytes.
    pub fn push_bytes(&mut self, value: &[u8]) {
        self.push(Some(value));
    }

    /// Appends a cell that is NULL when `value` is `None`.
    pub fn push_opt(&mut self, value: Option<&[u8]>) {
        self.push(value);
    }

    /// Number of complete rows so far.
    pub fn row_count(&self) -> usize {
        self.ends.len().checked_div(self.columns).unwrap_or(0)
    }

    /// Whether no cell was appended.
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Whether the batch should be finished before more rows are added.
    pub fn is_full(&self) -> bool {
        self.data.len() >= MAX_BATCH_BYTES
    }

    /// Takes the rows built so far, leaving the builder empty and reusable.
    ///
    /// # Panics
    /// Panics if the last row is incomplete: that is a driver bug.
    pub fn finish(&mut self) -> RowBatch {
        assert!(
            self.columns == 0 || self.ends.len().is_multiple_of(self.columns),
            "finish() called in the middle of a row"
        );
        RowBatch {
            columns: self.columns,
            ends: take(&mut self.ends),
            nulls: take(&mut self.nulls),
            data: take(&mut self.data),
        }
    }
}

/// All rows fetched so far for one result set, addressable by global row
/// index. Memory grows with what was fetched.
///
/// Cloning is cheap: the clone shares the rows (a snapshot of what was
/// fetched at that moment), so a result can be sorted or exported off the
/// UI thread while more rows arrive.
#[derive(Debug, Clone)]
pub struct ResultStore {
    columns: Arc<[Column]>,
    batches: Vec<Arc<RowBatch>>,
    /// `starts[i]` is the global index of the first row of `batches[i]`.
    starts: Vec<usize>,
    rows: usize,
}

impl ResultStore {
    /// An empty store for a result with these columns.
    pub fn new(columns: Arc<[Column]>) -> Self {
        Self {
            columns,
            batches: Vec::new(),
            starts: Vec::new(),
            rows: 0,
        }
    }

    /// The columns of the result.
    pub fn columns(&self) -> &Arc<[Column]> {
        &self.columns
    }

    /// Appends a batch of rows.
    pub fn push(&mut self, batch: RowBatch) {
        if batch.row_count() == 0 {
            return;
        }
        debug_assert_eq!(batch.column_count(), self.columns.len());
        self.starts.push(self.rows);
        self.rows += batch.row_count();
        self.batches.push(Arc::new(batch));
    }

    /// The same rows under other columns (as many): a result's rows as
    /// rows of the table its columns show, named as the table names them.
    /// Cheap, as a clone is.
    pub fn renamed(&self, columns: Arc<[Column]>) -> Self {
        debug_assert_eq!(columns.len(), self.columns.len());
        Self {
            columns,
            ..self.clone()
        }
    }

    /// Number of rows fetched so far.
    pub fn row_count(&self) -> usize {
        self.rows
    }

    fn locate(&self, row: usize) -> (&RowBatch, usize) {
        assert!(row < self.rows, "row {row} out of range ({})", self.rows);
        let batch = self.starts.partition_point(|&start| start <= row) - 1;
        (&self.batches[batch], row - self.starts[batch])
    }

    /// The bytes of a cell, or `None` for NULL.
    ///
    /// # Panics
    /// Panics if `row` or `column` is out of range.
    pub fn cell(&self, row: usize, column: usize) -> Option<&[u8]> {
        let (batch, local) = self.locate(row);
        batch.cell(local, column)
    }

    /// A cell as text, or `None` for NULL.
    pub fn text(&self, row: usize, column: usize) -> Option<&str> {
        let (batch, local) = self.locate(row);
        batch.text(local, column)
    }

    /// Heap bytes held by the stored rows, the store's own bookkeeping
    /// included.
    pub fn memory_bytes(&self) -> usize {
        // Each batch sits behind an `Arc`: two counters and the batch itself.
        let per_batch = size_of::<RowBatch>() + 2 * size_of::<usize>();
        let rows: usize = self.batches.iter().map(|batch| batch.memory_bytes()).sum();
        rows + self.batches.len() * per_batch
            + self.batches.capacity() * size_of::<Arc<RowBatch>>()
            + self.starts.capacity() * size_of::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(n: usize) -> Arc<[Column]> {
        (0..n)
            .map(|i| Column::new(format!("c{i}"), "text", TypeKind::Text))
            .collect()
    }

    #[test]
    fn null_is_distinct_from_empty() {
        let mut builder = BatchBuilder::new(3);
        builder.push_text("");
        builder.push_null();
        builder.push_text("x");
        let batch = builder.finish();
        assert_eq!(batch.row_count(), 1);
        assert_eq!(batch.text(0, 0), Some(""));
        assert_eq!(batch.text(0, 1), None);
        assert_eq!(batch.text(0, 2), Some("x"));
    }

    #[test]
    fn cells_round_trip_across_many_rows() {
        // More than 64 cells, so the NULL bitmap spans several words.
        let mut builder = BatchBuilder::new(4);
        for row in 0..100 {
            builder.push_text(&format!("r{row}"));
            if row % 3 == 0 {
                builder.push_null();
            } else {
                builder.push_bytes(&[row as u8, 0, 255]);
            }
            builder.push_text("");
            builder.push_text("é☃😀");
        }
        let batch = builder.finish();
        assert_eq!(batch.row_count(), 100);
        for row in 0..100 {
            assert_eq!(batch.text(row, 0), Some(format!("r{row}").as_str()));
            if row % 3 == 0 {
                assert_eq!(batch.cell(row, 1), None);
            } else {
                assert_eq!(batch.cell(row, 1), Some(&[row as u8, 0, 255][..]));
            }
            assert_eq!(batch.text(row, 2), Some(""));
            assert_eq!(batch.text(row, 3), Some("é☃😀"));
        }
    }

    #[test]
    fn builder_is_reusable_after_finish() {
        let mut builder = BatchBuilder::new(1);
        builder.push_text("a");
        let first = builder.finish();
        assert!(builder.is_empty());
        builder.push_null();
        builder.push_text("b");
        let second = builder.finish();
        assert_eq!(first.row_count(), 1);
        assert_eq!(second.row_count(), 2);
        assert_eq!(second.text(0, 0), None);
        assert_eq!(second.text(1, 0), Some("b"));
    }

    #[test]
    #[should_panic(expected = "middle of a row")]
    fn finish_rejects_a_partial_row() {
        let mut builder = BatchBuilder::new(2);
        builder.push_text("only one cell");
        builder.finish();
    }

    #[test]
    fn store_addresses_rows_across_batches() {
        let mut store = ResultStore::new(columns(2));
        let mut builder = BatchBuilder::new(2);
        let mut expected = Vec::new();
        for (batch, size) in [3usize, 1, 5].into_iter().enumerate() {
            for row in 0..size {
                let value = format!("b{batch}r{row}");
                builder.push_text(&value);
                builder.push_null();
                expected.push(value);
            }
            store.push(builder.finish());
        }
        // An empty batch must not disturb addressing.
        store.push(builder.finish());
        assert_eq!(store.row_count(), 9);
        for (row, value) in expected.iter().enumerate() {
            assert_eq!(store.text(row, 0), Some(value.as_str()));
            assert_eq!(store.cell(row, 1), None);
        }
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn store_rejects_rows_past_the_end() {
        let mut store = ResultStore::new(columns(1));
        let mut builder = BatchBuilder::new(1);
        builder.push_text("a");
        store.push(builder.finish());
        store.cell(1, 0);
    }

    /// What a batch of `cells` cells and `data` bytes must hold, no more.
    fn exact_bytes(cells: usize, data: usize) -> usize {
        cells * 4 + cells.div_ceil(64) * 8 + data
    }

    #[test]
    fn a_finished_batch_holds_exactly_its_cells() {
        let mut builder = BatchBuilder::new(3);
        for row in 0..1000 {
            builder.push_text(&format!("{row:06}"));
            builder.push_null();
            builder.push_bytes(b"abcd");
        }
        let big = builder.finish();
        assert_eq!(big.memory_bytes(), exact_bytes(3000, 1000 * 10));

        // The scratch kept from the big batch does not leak into the next.
        builder.push_text("x");
        builder.push_text("");
        builder.push_null();
        let small = builder.finish();
        assert_eq!(small.memory_bytes(), exact_bytes(3, 1));
        assert_eq!(small.text(0, 0), Some("x"));
        assert_eq!(small.text(0, 1), Some(""));
        assert_eq!(small.text(0, 2), None);
        assert_eq!(big.text(999, 0), Some("000999"));
    }

    #[test]
    fn a_scratch_grown_by_large_values_is_released() {
        let mut builder = BatchBuilder::new(1);
        builder.push_bytes(&vec![7; SCRATCH_KEPT_BYTES + 1]);
        let batch = builder.finish();
        assert_eq!(
            batch.cell(0, 0).map(<[u8]>::len),
            Some(SCRATCH_KEPT_BYTES + 1)
        );
        assert_eq!(builder.data.capacity(), 0);

        // An ordinary batch keeps its scratch for the next one.
        builder.push_text("kept");
        builder.finish();
        assert!(builder.data.capacity() >= 4);
    }

    #[test]
    fn store_memory_is_its_rows_plus_bookkeeping() {
        let mut store = ResultStore::new(columns(2));
        let mut builder = BatchBuilder::new(2);
        let mut cells_only = 0;
        for batch in 1..=10 {
            for _ in 0..batch {
                builder.push_text("abc");
                builder.push_null();
            }
            cells_only += exact_bytes(2 * batch, 3 * batch);
            store.push(builder.finish());
        }
        let held = store.memory_bytes();
        assert!(held > cells_only, "{held} <= {cells_only}");
        // Bookkeeping is a few hundred bytes per batch, never per row.
        assert!(held < cells_only + 10 * 256, "{held}");
    }

    #[test]
    fn zero_column_batches_are_empty() {
        let mut builder = BatchBuilder::new(0);
        assert_eq!(builder.row_count(), 0);
        assert_eq!(builder.finish().row_count(), 0);
    }

    #[test]
    fn a_clone_is_a_snapshot_that_shares_the_rows() {
        let columns: Arc<[Column]> = vec![Column::new("n", "int4", TypeKind::Integer)].into();
        let batch = |values: &[&str]| {
            let mut builder = BatchBuilder::new(1);
            for value in values {
                builder.push_text(value);
            }
            builder.finish()
        };
        let mut store = ResultStore::new(columns);
        store.push(batch(&["1", "2"]));
        let snapshot = store.clone();
        store.push(batch(&["3"]));
        assert_eq!((snapshot.row_count(), store.row_count()), (2, 3));
        assert_eq!(snapshot.text(1, 0), Some("2"));
        // The rows are the same memory, not a copy.
        assert!(std::ptr::eq(
            snapshot.cell(0, 0).unwrap().as_ptr(),
            store.cell(0, 0).unwrap().as_ptr()
        ));
    }
}
