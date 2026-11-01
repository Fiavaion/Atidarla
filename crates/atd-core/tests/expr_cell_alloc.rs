//! CLAUDE.md rule 2: evaluating `op('t')[row, col]` allocates nothing, by
//! index or by name, in a table or in text.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use atd_core::dat::{Dat, Table};
use atd_core::expr::{CellAt, Env, EvalError, Expr, cell_number};

struct Counting;
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}
// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

struct Frame {
    frame: f64,
    dats: [Dat; 2],
}

impl Env for Frame {
    fn time(&self) -> f64 {
        self.frame / 60.0
    }
    fn frame(&self) -> f64 {
        self.frame
    }
    fn me_digits(&self) -> Option<f64> {
        None
    }
    fn param(&self, node: usize, _: &str, _: Option<usize>) -> Result<f64, EvalError> {
        Err(EvalError::NoParam(node))
    }
    fn channel(&self, node: usize, _: &str) -> Result<f64, EvalError> {
        Err(EvalError::NoChannel(node))
    }
    fn cell(
        &self,
        node: usize,
        slot: usize,
        row: CellAt<'_>,
        col: CellAt<'_>,
    ) -> Result<f64, EvalError> {
        cell_number(&self.dats[node], slot, row, col)
    }
}

#[test]
fn cells_evaluate_without_allocating() {
    let table = Dat::table(Table::from_rows([
        ["name", "speed", "gain"],
        ["a", "0.5", "1"],
        ["b", "1.5", "2"],
        ["c", " 2.5 ", "3"],
    ]));
    let text = Dat::text("1\n2\n3\n");
    let sources = [
        "op('t')[1 + frame % 3, 1] + op('n')[frame % 3, 0]",
        "op('t')['b', 'speed'] * op('t')[1 + frame % 3, 'gain']",
        "op('t')[3, 'speed'] if frame % 2 else op('n')['2', 0]",
        // Errors cost nothing either.
        "op('t')[9, 'speed'] + op('t')[0, 'name']",
    ];
    let exprs: Vec<Expr> = sources.iter().map(|s| Expr::parse(s).unwrap()).collect();
    let mut env = Frame {
        frame: 0.0,
        dats: [table, text],
    };
    let mut sum = 0.0;
    COUNTING.with(|c| c.set(true));
    for f in 0..600 {
        env.frame = f64::from(f);
        for e in &exprs {
            sum += e.eval(&env).unwrap_or(0.0);
        }
    }
    COUNTING.with(|c| c.set(false));
    assert!(sum > 0.0);
    assert_eq!(ALLOCS.with(Cell::get), 0, "allocations");
}
