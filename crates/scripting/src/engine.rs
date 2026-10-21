//! The `ScriptEngine` wrapper: a locked-down raw Rhai engine + compiled `AST` +
//! `Scope`, stored as a Bevy `NonSend` resource (ADR-0011), with the Phase-12 limits (ADR-0046): a
//! source-size cap before compile, a wall-clock watchdog per call, and an operation and time budget
//! per frame across every call.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use rhai::packages::Package;
use rhai::{AST, Dynamic, Engine, EvalAltResult, Position, Scope};
// `std::time::Instant::now()` panics on wasm32; web-time is the same type natively and uses the
// browser clock on wasm32.
use web_time::Instant;

use crate::agent::{Agent, register_agent};
use crate::component::{Health, register_api};
use crate::error::ScriptError;

/// The most operations one script call may run (the infinite-loop guard).
pub const MAX_OPERATIONS_PER_CALL: u64 = 100_000;

/// The most elements an array may hold, nested elements included; this also bounds how deeply data
/// can nest (see `apply_limits`). Object maps are disabled; the same cap applies to them anyway.
pub const MAX_CONTAINER_ELEMENTS: usize = 128;

/// The deepest script call nesting (recursion included).
pub const MAX_CALL_LEVELS: usize = 16;

/// The deepest expression nesting at the top level of a script.
pub const MAX_EXPR_DEPTH: usize = 64;

/// The deepest expression nesting inside a function body.
pub const MAX_FN_EXPR_DEPTH: usize = 16;

/// The most functions one script may define (compile time grows faster than linearly with it).
pub const MAX_FUNCTIONS: usize = 128;

/// The longest string a script may build, in bytes.
pub const MAX_STRING_BYTES: usize = 1024;

/// The Phase-12 budgets (ADR-0046). The defaults bound a hostile script; they are not a frame-rate
/// target (see the escape-hatch trigger in ADR-0046 for that).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptLimits {
    /// The largest script source accepted, checked before parsing (parsing is not op-counted).
    pub max_source_bytes: usize,
    /// Wall-clock time one call may take. It catches what the operation counter cannot: a host
    /// function that blocks. Checked between operations.
    pub call_time_budget: Duration,
    /// Operations all calls together may run between two [`ScriptEngine::begin_frame`] calls.
    pub frame_ops_budget: u64,
    /// Wall-clock time all calls together may take between two `begin_frame` calls.
    pub frame_time_budget: Duration,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 256 * 1024,
            call_time_budget: Duration::from_millis(50),
            frame_ops_budget: 1_000_000,
            frame_time_budget: Duration::from_millis(100),
        }
    }
}

/// What the scripts cost in the current frame (since the last [`ScriptEngine::begin_frame`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Calls that ran (successfully or with a script error).
    pub calls: u32,
    /// Calls refused because the frame budget was spent.
    pub skipped: u32,
    /// Operations run.
    pub ops: u64,
    /// Wall-clock time spent in calls.
    pub elapsed: Duration,
}

/// State the `on_progress` watchdog shares with the engine (Rhai's callback must be `'static`).
#[derive(Default)]
struct Watchdog {
    /// When the current call must stop, if a call is running.
    deadline: Cell<Option<Instant>>,
    /// Operations the current call has run, counted by `on_progress` INVOCATIONS. Rhai's own counter
    /// misses array-package callbacks (they run in a cloned state whose operations it discards), but
    /// `on_progress` fires for every operation in every state, so this count is complete.
    ops: Cell<u64>,
    /// The most operations the current call may run: the per-call cap or what the frame has left.
    ops_cap: Cell<u64>,
}

/// The token an aborted call carries in `EvalAltResult::ErrorTerminated`.
pub const TIME_BUDGET_TOKEN: &str = "script time budget exceeded";
/// The token of a call aborted for running more operations than its cap allows (callbacks included).
pub const OPS_BUDGET_TOKEN: &str = "script operation budget exceeded";
/// The message of the error a call gets once the frame budget is spent.
pub const FRAME_BUDGET_TOKEN: &str = "script frame budget exhausted";

/// Per-tick script host. NOT `Send + Sync` (rhai's `sync` feature is OFF), so it is
/// stored via `world.insert_non_send_resource` — see [`crate::insert_scripting`].
pub struct ScriptEngine {
    engine: Engine,
    ast: AST,
    scope: Scope<'static>,
    scope_template_len: usize,
    source_path: Option<PathBuf>,
    /// Only ever stored and `==`-compared — NEVER `SystemTime::now()` (that panics
    /// on wasm32).
    last_modified: Option<SystemTime>,
    limits: ScriptLimits,
    watchdog: Rc<Watchdog>,
    frame: FrameStats,
}

impl std::fmt::Debug for ScriptEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately opaque about the engine internals; surface only the source.
        f.debug_struct("ScriptEngine")
            .field("source_path", &self.source_path)
            .field("last_modified", &self.last_modified)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// Read at most `limit` bytes of a script file; a larger file is refused without reading it all.
fn read_bounded(path: &Path, limit: usize) -> Result<String, ScriptError> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(ScriptError::TooLarge {
            bytes: bytes.len(),
            limit,
        });
    }
    String::from_utf8(bytes)
        .map_err(|e| ScriptError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

fn check_size(src: &str, limits: &ScriptLimits) -> Result<(), ScriptError> {
    if src.len() > limits.max_source_bytes {
        Err(ScriptError::TooLarge {
            bytes: src.len(),
            limit: limits.max_source_bytes,
        })
    } else {
        Ok(())
    }
}

impl ScriptEngine {
    /// Compile an in-memory script with the default [`ScriptLimits`] (no file hot-reload path).
    pub fn from_source(src: &str) -> Result<Self, ScriptError> {
        Self::from_source_with(src, ScriptLimits::default())
    }

    /// Compile an in-memory script with explicit limits.
    pub fn from_source_with(src: &str, limits: ScriptLimits) -> Result<Self, ScriptError> {
        Self::build(src, None, None, limits, |_| {})
    }

    /// Compile a script against the sandbox plus host functions `register` adds. Register only
    /// functions that are pure and bounded: the sandbox cannot interrupt a host function (the watchdog
    /// notices a slow one at the script's next operation), and the size check does not see inside
    /// custom types. Every limit and disabled symbol is applied AFTER `register`, so it cannot loosen
    /// them; hooks or custom syntax added here change the sandbox itself (ADR-0046).
    pub fn with_host_api(
        src: &str,
        limits: ScriptLimits,
        register: impl FnOnce(&mut Engine),
    ) -> Result<Self, ScriptError> {
        Self::build(src, None, None, limits, register)
    }

    /// Read + compile a script FILE (at most `max_source_bytes`, read bounded), seeding its mtime so
    /// [`Self::reload_if_changed`] only reloads on an actual change.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ScriptError> {
        let path = path.as_ref().to_path_buf();
        let limits = ScriptLimits::default();
        let src = read_bounded(&path, limits.max_source_bytes)?;
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Self::build(&src, Some(path), modified, limits, |_| {})
    }

    fn build(
        src: &str,
        source_path: Option<PathBuf>,
        last_modified: Option<SystemTime>,
        limits: ScriptLimits,
        register: impl FnOnce(&mut Engine),
    ) -> Result<Self, ScriptError> {
        check_size(src, &limits)?;
        let mut engine = Engine::new_raw();
        register_whitelist(&mut engine);
        register(&mut engine);
        // After the host callback, so nothing it did can loosen the sandbox.
        apply_limits(&mut engine);
        let watchdog = Rc::new(Watchdog::default());
        let dog = Rc::clone(&watchdog);
        engine.on_progress(move |_| {
            let ops = dog.ops.get() + 1;
            dog.ops.set(ops);
            if ops > dog.ops_cap.get() {
                return Some(Dynamic::from(OPS_BUDGET_TOKEN));
            }
            // Reading the clock costs ~20 ns; every 64 operations is plenty.
            if ops % 64 == 0
                && let Some(deadline) = dog.deadline.get()
                && Instant::now() >= deadline
            {
                return Some(Dynamic::from(TIME_BUDGET_TOKEN));
            }
            None
        });
        let ast = engine.compile(src)?;
        let scope = Scope::new();
        let scope_template_len = scope.len();
        Ok(Self {
            engine,
            ast,
            scope,
            scope_template_len,
            source_path,
            last_modified,
            limits,
            watchdog,
            frame: FrameStats::default(),
        })
    }

    /// The limits this engine enforces.
    pub fn limits(&self) -> ScriptLimits {
        self.limits
    }

    /// Start a new frame: the frame budget is available again. The host calls it once per tick,
    /// before the tick's script calls ([`crate::run_scripts`] and `engine_core::run_entity_scripts`
    /// do).
    pub fn begin_frame(&mut self) {
        self.frame = FrameStats::default();
    }

    /// What the scripts have cost since the last [`Self::begin_frame`].
    pub fn frame_stats(&self) -> FrameStats {
        self.frame
    }

    /// Whether this frame's budget is spent, so the next call would be refused.
    pub fn frame_budget_spent(&self) -> bool {
        self.frame.ops >= self.limits.frame_ops_budget
            || self.frame.elapsed >= self.limits.frame_time_budget
    }

    /// Run one guarded call: refuse it once the frame budget is spent, give it a deadline (the
    /// sooner of its own time budget and what the frame has left), and account its cost.
    fn guarded<T>(
        &mut self,
        call: impl FnOnce(&Engine, &mut Scope<'static>, &AST) -> Result<T, Box<EvalAltResult>>,
    ) -> Result<T, Box<EvalAltResult>> {
        let limits = self.limits;
        if self.frame_budget_spent() {
            self.frame.skipped += 1;
            return Err(Box::new(EvalAltResult::ErrorRuntime(
                Dynamic::from(FRAME_BUDGET_TOKEN),
                Position::NONE,
            )));
        }
        let started = Instant::now();
        let frame_left = limits.frame_time_budget.saturating_sub(self.frame.elapsed);
        // `checked_add`: a host may configure a budget near `Duration::MAX`; then there is no deadline.
        self.watchdog
            .deadline
            .set(started.checked_add(limits.call_time_budget.min(frame_left)));
        self.watchdog.ops.set(0);
        self.watchdog.ops_cap.set(
            MAX_OPERATIONS_PER_CALL.min(limits.frame_ops_budget.saturating_sub(self.frame.ops)),
        );
        let ScriptEngine {
            engine,
            ast,
            scope,
            scope_template_len,
            ..
        } = self;
        let out = call(engine, scope, ast);
        scope.rewind(*scope_template_len);
        self.watchdog.deadline.set(None);
        self.frame.calls += 1;
        self.frame.ops += self.watchdog.ops.get();
        self.frame.elapsed += started.elapsed();
        out
    }

    /// Swap the compiled script in memory (keeps the `Engine` + its registrations).
    /// A script that fails to compile leaves the last-good `AST` in place.
    pub fn reload_from_str(&mut self, src: &str) -> Result<(), ScriptError> {
        check_size(src, &self.limits)?;
        let ast = self.engine.compile(src)?;
        self.ast = ast;
        self.scope.clear();
        self.scope_template_len = self.scope.len();
        Ok(())
    }

    /// Native file hot-reload: recompile if the source file's mtime changed.
    /// WASM-safe — with no filesystem, `metadata` errors and this is a clean no-op
    /// (`Ok(false)`); it never calls `SystemTime::now()`.
    pub fn reload_if_changed(&mut self) -> Result<bool, ScriptError> {
        let Some(path) = self.source_path.clone() else {
            return Ok(false);
        };
        let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            return Ok(false);
        };
        if self.last_modified == Some(modified) {
            return Ok(false);
        }
        let src = read_bounded(&path, self.limits.max_source_bytes)?;
        self.reload_from_str(&src)?;
        self.last_modified = Some(modified);
        Ok(true)
    }

    /// Run the script's `update(component)` once. Rhai passes the component by value;
    /// the script returns the mutated copy, which we hand back. The `Scope` is rewound
    /// after the call so no state leaks between entities or ticks. The call counts against the
    /// frame budget and is aborted by the watchdog past its time budget.
    pub fn update_component(&mut self, c: Health) -> Result<Health, Box<EvalAltResult>> {
        self.guarded(|engine, scope, ast| engine.call_fn::<Health>(scope, ast, "update", (c,)))
    }

    /// Run the script's `update(agent)` once for one scripted entity (ADR-0045). Like
    /// [`Self::update_component`]: the agent goes in by value, the returned copy comes back, the
    /// `Scope` is rewound, and the frame budget and the watchdog apply. A script that returns
    /// anything other than an `Agent` is an error.
    pub fn update_agent(&mut self, a: Agent) -> Result<Agent, Box<EvalAltResult>> {
        self.guarded(|engine, scope, ast| engine.call_fn::<Agent>(scope, ast, "update", (a,)))
    }
}

/// The whitelisted API, and nothing else: the array package (a bounded script data type), the
/// `Health` example component, and the `Agent` API.
fn register_whitelist(engine: &mut Engine) {
    // Arrays are a bounded script data type; register just the array package so
    // `set_max_array_size` has something to guard. Numeric/bool/string operators are
    // built-in even in a raw engine — no package needed for `h.hp += 1`.
    engine.register_global_module(rhai::packages::BasicArrayPackage::new().as_shared_module());
    // `for i in a..b` and `a..=b`: the raw engine has no range iterator. Iterating a range allocates
    // nothing, and each step is an operation (ADR-0046).
    engine.register_iterator::<std::ops::Range<rhai::INT>>();
    engine.register_iterator::<std::ops::RangeInclusive<rhai::INT>>();
    register_api(engine);
    register_agent(engine);
}

/// Every resource limit, and the lockdown. Applied last, after any host registration.
fn apply_limits(engine: &mut Engine) {
    // For operations/string/array/map, a value of 0 would mean UNLIMITED, so we set
    // real caps. `set_max_modules(0)` is the exception: there 0 means ZERO modules
    // allowed (the first `import` fails), which is what we want.
    engine.set_max_operations(MAX_OPERATIONS_PER_CALL); // > 0 REQUIRED — the infinite-loop guard
    // Native stack use grows with call levels x expression depth per body, plus the top-level depth
    // and the container nesting at the bottom; at 16 x 16 the worst case (`worst_case_stack_scripts`)
    // needs under half of a 1 MiB stack in release.
    engine.set_max_call_levels(MAX_CALL_LEVELS);
    // With the element cap, this bounds every checked value, and so what one assignment can add
    // (`tests/memory.rs`).
    engine.set_max_string_size(MAX_STRING_BYTES);
    // Rhai counts nested elements against these, so they also cap NESTING depth. Measured
    // (ADR-0046): Rhai's recursive size walk needs ~1.5 KiB of native stack per nested level in a debug
    // build (<0.5 KiB in release); 1024 levels overflowed a 2 MiB thread and so aborted the process.
    // 128, with the call and expression caps, keeps the combined worst case inside a 1 MiB release
    // stack (`worst_case_stack_scripts`, `just stack-check`).
    engine.set_max_array_size(MAX_CONTAINER_ELEMENTS);
    engine.set_max_map_size(MAX_CONTAINER_ELEMENTS);
    // (expression, fn-body expression) — parse-time. Stricter than Phase 12's suggested (64, 64).
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_FN_EXPR_DEPTH);
    engine.set_max_modules(0); // 0 == NO modules (not unlimited) — first `import` fails
    // Parsing is not op-counted, and compile time grows faster than linearly with function count.
    engine.set_max_functions(MAX_FUNCTIONS);
    // A host must not raise this: `Full` evaluates pure host functions at compile time, outside the
    // watchdog.
    engine.set_optimization_level(rhai::OptimizationLevel::Simple);
    engine.disable_symbol("eval"); // `eval` becomes a parse-time error
    engine.set_allow_anonymous_fn(false); // thin scripts; no closures for now
    // No object maps and no function pointers (ADR-0046). Rhai's size check counts neither map keys
    // nor the name and curried arguments a function pointer carries, and assigning a new map key by
    // index skips the map-size check: measured, one call reached ~1 GB by cloning a 127-key map into
    // new keys, and `p = p.curry(p)` doubled until the allocator failed and the process aborted.
    // Without them every value is counted, and what one call can build is bounded by the operation
    // cap (`tests/memory.rs`). Callbacks by name (`a.map("f")`) still work.
    for symbol in ["#{", "Fn", "call", "curry"] {
        engine.disable_symbol(symbol);
    }
}

/// Build the locked-down raw engine: nothing by default, an explicit whitelist,
/// every resource limit set, `eval` disabled. No eval / filesystem / network. Public so a test can
/// enumerate the exact whitelist (and a host can inspect it); scripts cannot reach it. The
/// per-call watchdog and the frame budget live in [`ScriptEngine`].
pub fn sandbox_engine() -> Engine {
    let mut engine = Engine::new_raw();
    register_whitelist(&mut engine);
    apply_limits(&mut engine);
    engine
}

/// The scripts that drive native recursion deepest within the limits (ADR-0046), by name. Each nests
/// a top-level expression as deep as the parser accepts (top-level statements run on every call)
/// around a recursion to the call-depth cap, nests every function body as deep as it accepts with
/// the recursive call innermost, and at the bottom compares two arrays nested to the container cap
/// with `==`. Measured (2026-10-21, x86-64): together they need at most 448 KiB of native stack in
/// release and 2.6 MiB in debug. The stack tests (4 MiB, debug) and `examples/stack_check.rs`
/// (`just stack-check`: 1 MiB, release) run all of them.
pub fn worst_case_stack_scripts() -> Vec<(&'static str, String)> {
    // (name, opening, closing, the recursive call)
    const SHAPES: [(&str, &str, &str, &str); 3] = [
        ("parens", "(", " + 1)", "deep(a, b, n - 1)"),
        (
            "switch",
            "switch 0 { 0 => ",
            ", _ => 0 }",
            "deep(a, b, n - 1)",
        ),
        ("index", "[", "][0]", "deep(a, b, n - 1)"),
    ];
    let engine = sandbox_engine();
    let deepest = |max: usize, src: &dyn Fn(usize) -> String| {
        (0..=max)
            .rev()
            .find(|&depth| engine.compile(src(depth)).is_ok())
            .unwrap_or(0)
    };
    SHAPES
        .iter()
        .map(|&(name, open, close, call)| {
            let nest = |depth: usize, inner: &str| {
                format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
            };
            let build = |body: usize, top: usize| {
                format!(
                    "fn deep(a, b, n) {{ if n <= 0 {{ return if a == b {{ 1 }} else {{ 0 }}; }} {} }}\n\
                     fn update(h) {{ h }}\n\
                     let a = 0; let b = 0; let i = 0;\n\
                     while i < {} {{ a = [a]; b = [b]; i += 1; }}\n\
                     let r = {};",
                    nest(body, call),
                    MAX_CONTAINER_ELEMENTS - 2,
                    nest(top, &format!("deep(a, b, {})", MAX_CALL_LEVELS - 1)),
                )
            };
            let body = deepest(MAX_FN_EXPR_DEPTH, &|depth| build(depth, 0));
            let top = deepest(MAX_EXPR_DEPTH, &|depth| build(body, depth));
            (name, build(body, top))
        })
        .collect()
}
