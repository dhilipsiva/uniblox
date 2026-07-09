//! The `ScriptEngine` wrapper: a locked-down raw Rhai engine + compiled `AST` +
//! `Scope`, stored as a Bevy `NonSend` resource (ADR-0011).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rhai::packages::Package;
use rhai::{AST, Engine, EvalAltResult, Scope};

use crate::component::{Health, register_api};
use crate::error::ScriptError;

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
}

impl std::fmt::Debug for ScriptEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately opaque about the engine internals; surface only the source.
        f.debug_struct("ScriptEngine")
            .field("source_path", &self.source_path)
            .field("last_modified", &self.last_modified)
            .finish_non_exhaustive()
    }
}

impl ScriptEngine {
    /// Compile an in-memory script (no file hot-reload path).
    pub fn from_source(src: &str) -> Result<Self, ScriptError> {
        Self::build(src, None, None)
    }

    /// Read + compile a script FILE, seeding its mtime so [`Self::reload_if_changed`]
    /// only reloads on an actual change.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ScriptError> {
        let path = path.as_ref().to_path_buf();
        let src = std::fs::read_to_string(&path)?;
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Self::build(&src, Some(path), modified)
    }

    fn build(
        src: &str,
        source_path: Option<PathBuf>,
        last_modified: Option<SystemTime>,
    ) -> Result<Self, ScriptError> {
        let engine = build_engine();
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
        })
    }

    /// Swap the compiled script in memory (keeps the `Engine` + its registrations).
    /// A script that fails to compile leaves the last-good `AST` in place.
    pub fn reload_from_str(&mut self, src: &str) -> Result<(), ScriptError> {
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
        let src = std::fs::read_to_string(&path)?;
        self.reload_from_str(&src)?;
        self.last_modified = Some(modified);
        Ok(true)
    }

    /// Run the script's `update(component)` once. Rhai passes the component by value;
    /// the script returns the mutated copy, which we hand back. The `Scope` is rewound
    /// after the call so no state leaks between entities or ticks.
    pub fn update_component(&mut self, c: Health) -> Result<Health, Box<EvalAltResult>> {
        // Disjoint field borrows so `call_fn` can hold `&engine` + `&mut scope` + `&ast`
        // at once (`self.engine.call_fn(&mut self.scope, ..)` would overlap-borrow self).
        let ScriptEngine {
            engine,
            ast,
            scope,
            scope_template_len,
            ..
        } = self;
        let out = engine.call_fn::<Health>(scope, ast, "update", (c,));
        scope.rewind(*scope_template_len);
        out
    }
}

/// Build the locked-down raw engine: nothing by default, an explicit whitelist,
/// every resource limit set, `eval` disabled. No eval / filesystem / network.
fn build_engine() -> Engine {
    let mut engine = Engine::new_raw();

    // Arrays are a bounded script data type; register just the array package so
    // `set_max_array_size` has something to guard. Numeric/bool/string operators are
    // built-in even in a raw engine — no package needed for `h.hp += 1`.
    engine.register_global_module(rhai::packages::BasicArrayPackage::new().as_shared_module());

    // ── resource limits (ALL set) ──
    // For operations/string/array/map, a value of 0 would mean UNLIMITED, so we set
    // real caps. `set_max_modules(0)` is the exception: there 0 means ZERO modules
    // allowed (the first `import` fails), which is what we want.
    engine.set_max_operations(100_000); // > 0 REQUIRED — the infinite-loop guard
    engine.set_max_call_levels(32); // recursion depth (below the default 64)
    engine.set_max_string_size(8 * 1024);
    engine.set_max_array_size(1024);
    engine.set_max_map_size(1024);
    engine.set_max_expr_depths(64, 32); // (expression, fn-body expression) — parse-time
    engine.set_max_modules(0); // 0 == NO modules (not unlimited) — first `import` fails

    // ── lockdown ──
    engine.disable_symbol("eval"); // `eval` becomes a parse-time error
    engine.set_allow_anonymous_fn(false); // thin scripts; no closures for now

    register_api(&mut engine);
    engine
}
