use crate::{Call, Function, Inventory, Location, Observation, SourceFile, digest};
use proc_macro2::{Span, TokenStream, TokenTree};
use quote::ToTokens;
use std::{collections::BTreeMap, fs, path::Path};
use syn::{Expr, FnArg, Pat, Type, spanned::Spanned, visit::Visit};

fn tokens(value: &impl ToTokens) -> String {
    value.to_token_stream().to_string()
}

fn type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => Some(
            path.path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::"),
        ),
        Type::Reference(reference) => type_name(&reference.elem),
        _ => None,
    }
}

fn imports(items: &[syn::Item]) -> BTreeMap<String, String> {
    fn flatten(tree: &syn::UseTree, prefix: &str, result: &mut BTreeMap<String, String>) {
        match tree {
            syn::UseTree::Path(path) => {
                flatten(&path.tree, &format!("{prefix}{}::", path.ident), result)
            }
            syn::UseTree::Name(name) => {
                result.insert(name.ident.to_string(), format!("{prefix}{}", name.ident));
            }
            syn::UseTree::Rename(rename) => {
                result.insert(
                    rename.rename.to_string(),
                    format!("{prefix}{}", rename.ident),
                );
            }
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    flatten(tree, prefix, result);
                }
            }
            syn::UseTree::Glob(_) => {}
        }
    }
    let mut result = BTreeMap::new();
    for item in items {
        if let syn::Item::Use(item) = item
            && !test_only(&item.attrs)
        {
            flatten(&item.tree, "", &mut result);
        }
    }
    result
}

fn expand_import(path: &str, imports: &BTreeMap<String, String>) -> String {
    let (first, rest) = path.split_once("::").unwrap_or((path, ""));
    match imports.get(first) {
        Some(full) if rest.is_empty() => full.clone(),
        Some(full) => format!("{full}::{rest}"),
        None => path.into(),
    }
}

// cfg(test) is false in every production build. Other cfg conditions are kept
// conservatively, including cfg(any(test, feature = "...")) and platform cfgs.
fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || (attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Path>()
                    .is_ok_and(|p| p.is_ident("test")))
    })
}

pub fn scan(root: &Path) -> Inventory {
    let mut inventory = Inventory::default();
    for subtree in ["crates", "modules"] {
        let path = root.join(subtree);
        if !path.is_dir() {
            inventory
                .errors
                .push(format!("missing source root: {subtree}"));
        } else {
            walk(root, &path, &mut inventory);
        }
    }
    if let Ok(modules) = fs::read_to_string(root.join(".gitmodules")) {
        for path in modules
            .lines()
            .filter_map(|line| line.trim().strip_prefix("path = "))
        {
            if path.starts_with("modules/")
                && !inventory
                    .files
                    .iter()
                    .any(|f| f.path.starts_with(&format!("{path}/")))
            {
                inventory
                    .gaps
                    .push(format!("unavailable module source: {path}"));
            }
        }
    }
    inventory.functions.sort_by(|a, b| a.id.cmp(&b.id));
    inventory.files.sort_by(|a, b| a.path.cmp(&b.path));
    inventory.excluded.sort();
    inventory.gaps.sort();
    inventory.errors.sort();
    inventory
}

fn walk(root: &Path, directory: &Path, inventory: &mut Inventory) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            inventory
                .errors
                .push(format!("{}: {error}", directory.display()));
            return;
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(error) => inventory.errors.push(error.to_string()),
        }
    }
    paths.sort();
    for path in paths {
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let name = path.file_name().unwrap().to_string_lossy();
        if path.is_symlink() {
            inventory
                .gaps
                .push(format!("symlink not followed: {relative}"));
            continue;
        }
        if path.is_dir() {
            if [
                "target",
                ".git",
                "tests",
                "test-support",
                "examples",
                "benches",
                "third-party",
                "fixtures",
                "node_modules",
                "test_support",
            ]
            .contains(&name.as_ref())
            {
                inventory.excluded.push(relative);
            } else {
                walk(root, &path, inventory);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            if name == "build.rs"
                || name == "tests.rs"
                || name.ends_with("_tests.rs")
                || name.starts_with("test_")
            {
                inventory.excluded.push(relative);
                continue;
            }
            match fs::read_to_string(&path) {
                Ok(source) => {
                    let package = package_for(root, &path);
                    match parse_source(&relative, &package, &source) {
                        Ok(functions) => {
                            inventory.files.push(SourceFile {
                                path: relative,
                                sha256: digest(source.as_bytes()),
                                functions: functions.len(),
                            });
                            inventory.functions.extend(functions);
                        }
                        Err(error) => inventory.errors.push(format!("{relative}: {error}")),
                    }
                }
                Err(error) => inventory.errors.push(format!("{relative}: {error}")),
            }
        } else if name == "api.json" && relative.contains("/metadata/") {
            match fs::read_to_string(&path).and_then(|source| {
                serde_json::from_str::<serde_json::Value>(&source).map_err(std::io::Error::other)
            }) {
                Ok(apis) => inventory
                    .api_metadata
                    .push(serde_json::json!({"file":relative,"apis":apis})),
                Err(error) => inventory.errors.push(format!("{relative}: {error}")),
            }
        }
    }
}

fn package_for(root: &Path, file: &Path) -> String {
    for parent in file.ancestors().skip(1) {
        if let Ok(manifest) = fs::read_to_string(parent.join("Cargo.toml")) {
            let mut package = false;
            for line in manifest.lines().map(str::trim) {
                if line.starts_with('[') {
                    package = line == "[package]";
                }
                if package
                    && let Some(name) = line
                        .strip_prefix("name")
                        .and_then(|s| s.trim().strip_prefix('='))
                {
                    return name.trim().trim_matches('"').replace('-', "_");
                }
            }
        }
        if parent == root {
            break;
        }
    }
    "unknown".into()
}

pub fn parse_source(file: &str, package: &str, source: &str) -> Result<Vec<Function>, syn::Error> {
    let syntax = syn::parse_file(source)?;
    let namespace = file
        .split("/src/")
        .nth(1)
        .unwrap_or(file)
        .trim_end_matches(".rs")
        .replace('/', "::");
    let imports = imports(&syntax.items);
    let mut collector = Functions {
        file,
        package,
        source,
        namespace,
        owner: String::new(),
        imports,
        result: Vec::new(),
    };
    collector.visit_file(&syntax);
    Ok(collector.result)
}

struct Functions<'a> {
    file: &'a str,
    package: &'a str,
    source: &'a str,
    namespace: String,
    owner: String,
    imports: BTreeMap<String, String>,
    result: Vec<Function>,
}

impl Functions<'_> {
    fn function(&mut self, signature: &syn::Signature, block: &syn::Block) {
        let name = signature.ident.to_string();
        let start = signature.ident.span().start();
        let mut types = BTreeMap::new();
        for arg in &signature.inputs {
            if let FnArg::Typed(arg) = arg
                && let Pat::Ident(binding) = &*arg.pat
                && let Some(ty) = type_name(&arg.ty)
            {
                types.insert(binding.ident.to_string(), expand_import(&ty, &self.imports));
            }
        }
        types.insert("self".into(), self.owner.clone());
        let mut body = Body {
            file: self.file,
            source: self.source,
            guards: Vec::new(),
            observations: Vec::new(),
            calls: Vec::new(),
            types,
            imports: self.imports.clone(),
        };
        body.visit_block(block);
        self.result.push(Function {
            id: format!("{}:{}:{}::{}", self.file, start.line, self.owner, name),
            name,
            owner: self.owner.clone(),
            namespace: self.namespace.clone(),
            package: self.package.into(),
            location: body.location(signature.ident.span()),
            observations: body.observations,
            calls: body.calls,
        });
        let previous = self.namespace.clone();
        let previous_owner = std::mem::take(&mut self.owner);
        self.namespace = format!("{}::fn_{}", self.namespace, signature.ident);
        self.visit_block(block);
        self.namespace = previous;
        self.owner = previous_owner;
    }
}

impl<'ast> Visit<'ast> for Functions<'_> {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !test_only(&item.attrs) {
            self.function(&item.sig, &item.block);
        }
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if test_only(&item.attrs) {
            return;
        }
        let previous = std::mem::replace(
            &mut self.owner,
            type_name(&item.self_ty)
                .unwrap_or_else(|| tokens(&item.self_ty))
                .split("::")
                .last()
                .unwrap_or("")
                .into(),
        );
        syn::visit::visit_item_impl(self, item);
        self.owner = previous;
    }
    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !test_only(&item.attrs) {
            self.function(&item.sig, &item.block);
        }
    }
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if test_only(&item.attrs) {
            return;
        }
        let previous = self.namespace.clone();
        let previous_imports = self.imports.clone();
        if let Some((_, items)) = &item.content {
            self.imports.extend(imports(items));
        }
        self.namespace = format!("{}::{}", self.namespace, item.ident);
        syn::visit::visit_item_mod(self, item);
        self.namespace = previous;
        self.imports = previous_imports;
    }
    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if !test_only(&item.attrs)
            && let Some(block) = &item.default
        {
            self.function(&item.sig, block);
        }
    }
}

struct Body<'a> {
    file: &'a str,
    source: &'a str,
    guards: Vec<String>,
    observations: Vec<Observation>,
    calls: Vec<Call>,
    types: BTreeMap<String, String>,
    imports: BTreeMap<String, String>,
}

impl Body<'_> {
    fn location(&self, span: Span) -> Location {
        Location {
            file: self.file.into(),
            line: span.start().line,
            column: span.start().column + 1,
        }
    }
    fn observe(&mut self, kind: &str, node: &impl ToTokens, span: Span) {
        let line = span.start().line;
        let evidence = self
            .source
            .lines()
            .skip(line.saturating_sub(1))
            .take((span.end().line.saturating_sub(line) + 1).min(8))
            .collect::<Vec<_>>()
            .join("\n");
        self.observations.push(Observation {
            kind: kind.into(),
            location: self.location(span),
            expression: tokens(node),
            evidence,
            guards: self.guards.clone(),
            literals: literals(node.to_token_stream()),
        });
    }
    fn call(
        &mut self,
        name: String,
        qualifier: String,
        receiver_type: Option<String>,
        method: bool,
        span: Span,
    ) {
        self.calls.push(Call {
            name,
            qualifier,
            receiver_type,
            method,
            location: self.location(span),
        });
    }
    fn macro_observation(&mut self, mac: &syn::Macro, node: &impl ToTokens, span: Span) {
        let name = mac.path.segments.last().unwrap().ident.to_string();
        if [
            "panic",
            "todo",
            "unimplemented",
            "assert",
            "assert_eq",
            "assert_ne",
        ]
        .contains(&name.as_str())
        {
            self.observe("panic_candidate", node, span);
        }
        if name == "json" && contains_false_ok(mac.tokens.clone()) {
            self.observe("negative_response", node, span);
        }
        if ![
            "panic",
            "todo",
            "unimplemented",
            "assert",
            "assert_eq",
            "assert_ne",
            "json",
            "format",
            "format_args",
            "println",
            "print",
            "eprintln",
            "eprint",
            "write",
            "writeln",
            "vec",
            "matches",
            "warn",
            "info",
            "debug",
            "trace",
            "error",
            "concat",
            "env",
            "option_env",
            "stringify",
            "file",
            "line",
            "column",
            "include_str",
            "include_bytes",
        ]
        .contains(&name.as_str())
        {
            self.observe("opaque_macro_boundary", node, span);
        }
    }
}

/// Extract literals from parsed token trees, never comments or arbitrary text.
pub fn literals(stream: TokenStream) -> Vec<String> {
    let mut result = Vec::new();
    for tree in stream {
        match tree {
            TokenTree::Group(group) => result.extend(literals(group.stream())),
            TokenTree::Literal(literal) => {
                if let Ok(syn::Lit::Str(value)) = syn::parse_str::<syn::Lit>(&literal.to_string()) {
                    result.push(value.value());
                }
            }
            _ => {}
        }
    }
    result
}

fn sink_kind(name: &str) -> Option<&'static str> {
    if [
        "fail",
        "failed",
        "fail_record",
        "fail_start",
        "fail_network_action",
        "refuse_open",
        "reject",
        "rejected",
        "reject_resume",
        "reject_start",
        "reject_open",
        "fail_before_resume_stream",
    ]
    .contains(&name)
    {
        Some("failure_boundary")
    } else if [
        "close",
        "close_with_cause",
        "stop",
        "terminate",
        "shutdown",
        "retire",
        "revoke_ticket",
        "begin_cleanup",
        "begin_stop",
        "cancel",
        "abort",
        "disconnect",
        "kill",
        "exit",
    ]
    .contains(&name)
    {
        Some("termination_candidate")
    } else if [
        "error",
        "request_error",
        "private_error",
        "public_error",
        "error_response",
        "refusal",
        "reject_reply",
    ]
    .contains(&name)
    {
        Some("error_reply_candidate")
    } else if ["reply_status", "status_reply"].contains(&name) {
        Some("wire_status_candidate")
    } else {
        None
    }
}

fn module_boundary(name: &str) -> bool {
    [
        "request",
        "request_api",
        "send",
        "send_ephemeral",
        "call_direct",
        "request_direct",
        "processing_api",
        "complete",
        "respond",
        "reply",
    ]
    .contains(&name)
}

impl<'ast> Visit<'ast> for Body<'_> {
    // Nested declarations are independently scoped, not executed by the outer function.
    fn visit_item(&mut self, _item: &'ast syn::Item) {}
    fn visit_expr(&mut self, node: &'ast Expr) {
        if test_only(node.attrs()) {
            return;
        }
        syn::visit::visit_expr(self, node);
    }
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Expr::Path(path) = &*node.func {
            let expanded = expand_import(
                &path
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::"),
                &self.imports,
            );
            let (qualifier, name) = expanded.rsplit_once("::").unwrap_or(("", &expanded));
            let qualifier = qualifier.to_string();
            let name = name.to_string();
            if name == "Err" {
                self.observe("error_origin", node, node.span());
            }
            if let Some(kind) = sink_kind(&name)
                && (kind != "wire_status_candidate" || !success_status(&node.args))
            {
                self.observe(kind, node, node.span());
            }
            if ["critical", "demoted", "advisory"].contains(&name.as_str())
                && qualifier.contains("StreamIssue")
            {
                self.observe("declared_disposition", node, node.span());
            }
            if module_boundary(&name) || (name == "new" && qualifier.ends_with("Pending")) {
                self.observe("module_boundary", node, node.span());
            }
            self.call(name, qualifier, None, false, node.span());
        } else {
            self.observe("indirect_dispatch", node, node.span());
        }
        syn::visit::visit_expr_call(self, node);
    }
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let name = node.method.to_string();
        let receiver = tokens(&node.receiver);
        let receiver_type = if let Expr::Path(path) = &*node.receiver {
            path.path
                .get_ident()
                .and_then(|id| self.types.get(&id.to_string()).cloned())
        } else {
            None
        };
        if let Some(kind) = sink_kind(&name)
            && (kind != "wire_status_candidate" || !success_status(&node.args))
        {
            self.observe(kind, node, node.span());
        }
        if ["map_err", "ok_or", "ok_or_else"].contains(&name.as_str()) {
            self.observe("error_conversion", node, node.span());
        }
        if ["unwrap", "expect"].contains(&name.as_str()) {
            self.observe("panic_candidate", node, node.span());
        }
        if module_boundary(&name) {
            self.observe("module_boundary", node, node.span());
        }
        self.call(name, receiver, receiver_type, true, node.span());
        syn::visit::visit_expr_method_call(self, node);
    }
    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        self.observe("error_propagation", node, node.span());
        syn::visit::visit_expr_try(self, node);
    }
    fn visit_expr_assign(&mut self, node: &'ast syn::ExprAssign) {
        if failure_state(&node.right) {
            self.observe("lifecycle_transition", node, node.span());
        }
        syn::visit::visit_expr_assign(self, node);
    }
    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        // json! is opaque to syn. Inspect parsed token pairs only; report this
        // evidence as a candidate, without pretending to expand the macro.
        self.macro_observation(&node.mac, node, node.span());
        syn::visit::visit_expr_macro(self, node);
    }
    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        if !test_only(&node.attrs) {
            self.macro_observation(&node.mac, node, node.span());
        }
    }
    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.visit_expr(&node.cond);
        self.guards.push(format!("if {}", tokens(&node.cond)));
        self.visit_block(&node.then_branch);
        self.guards.pop();
        if let Some((_, branch)) = &node.else_branch {
            self.guards.push(format!("else of {}", tokens(&node.cond)));
            self.visit_expr(branch);
            self.guards.pop();
        }
    }
    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        self.visit_expr(&node.expr);
        for arm in &node.arms {
            if test_only(&arm.attrs) {
                continue;
            }
            let guard = arm
                .guard
                .as_ref()
                .map(|(_, e)| format!(" if {}", tokens(e)))
                .unwrap_or_default();
            self.guards.push(format!(
                "match {} => {}{guard}",
                tokens(&node.expr),
                tokens(&arm.pat)
            ));
            if let Some((_, condition)) = &arm.guard {
                self.visit_expr(condition);
            }
            self.visit_expr(&arm.body);
            self.guards.pop();
        }
    }
    fn visit_local(&mut self, node: &'ast syn::Local) {
        if test_only(&node.attrs) {
            return;
        }
        if let Pat::Ident(binding) = &node.pat {
            self.types.remove(&binding.ident.to_string());
        }
        if let Pat::Type(pat) = &node.pat
            && let Pat::Ident(binding) = &*pat.pat
            && let Some(ty) = type_name(&pat.ty)
        {
            self.types
                .insert(binding.ident.to_string(), expand_import(&ty, &self.imports));
        }
        if let Some(init) = &node.init
            && init.diverge.is_some()
        {
            self.guards.push(format!("let-else {}", tokens(&node.pat)));
            syn::visit::visit_local(self, node);
            self.guards.pop();
        } else {
            syn::visit::visit_local(self, node);
        }
    }
    fn visit_expr_return(&mut self, node: &'ast syn::ExprReturn) {
        if let Some(expr) = &node.expr
            && sentinel(expr)
        {
            self.observe("sentinel_return_candidate", node, node.span());
        }
        if let Some(expr) = &node.expr
            && failure_state(expr)
        {
            self.observe("failure_state_return_candidate", node, node.span());
        }
        syn::visit::visit_expr_return(self, node);
    }
    fn visit_block(&mut self, node: &'ast syn::Block) {
        let previous = self.types.clone();
        if let Some(syn::Stmt::Expr(expr, None)) = node.stmts.last()
            && sentinel(expr)
        {
            self.observe("sentinel_return_candidate", expr, expr.span());
        }
        if let Some(syn::Stmt::Expr(expr, None)) = node.stmts.last()
            && failure_state(expr)
        {
            self.observe("failure_state_return_candidate", expr, expr.span());
        }
        syn::visit::visit_block(self, node);
        self.types = previous;
    }
    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        self.guards.push(format!(
            "closure {} (callback execution/return boundary unresolved)",
            tokens(&node.inputs)
        ));
        syn::visit::visit_expr_closure(self, node);
        self.guards.pop();
    }
    fn visit_expr_async(&mut self, node: &'ast syn::ExprAsync) {
        self.guards
            .push("async block (execution/cancellation boundary unresolved)".into());
        syn::visit::visit_expr_async(self, node);
        self.guards.pop();
    }
    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        if node.fields.iter().any(|f| matches!(&f.member, syn::Member::Named(name) if ["status", "status_code"].contains(&name.to_string().as_str())) && negative_status(&f.expr)) { self.observe("negative_wire_structure", node, node.span()); }
        syn::visit::visit_expr_struct(self, node);
    }
}

fn negative_status(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(n) if matches!(&n.lit, syn::Lit::Int(i) if i.base10_parse::<u16>().is_ok_and(|v| (400..600).contains(&v))))
}

fn failure_state(expr: &Expr) -> bool {
    let tail = |block: &syn::Block| matches!(block.stmts.last(), Some(syn::Stmt::Expr(expr, None)) if failure_state(expr));
    match expr {
        Expr::Path(path) => {
            path.path.segments.len() > 1
                && path.path.segments.last().is_some_and(|s| {
                    [
                        "Failed",
                        "Stopping",
                        "Closing",
                        "Closed",
                        "Cancelled",
                        "CleanupFailed",
                        "Retiring",
                        "Rejected",
                    ]
                    .contains(&s.ident.to_string().as_str())
                })
        }
        Expr::If(branch) => {
            tail(&branch.then_branch)
                || branch
                    .else_branch
                    .as_ref()
                    .is_some_and(|(_, expr)| failure_state(expr))
        }
        Expr::Match(branch) => branch.arms.iter().any(|arm| failure_state(&arm.body)),
        Expr::Block(block) => tail(&block.block),
        Expr::Paren(paren) => failure_state(&paren.expr),
        _ => false,
    }
}

fn sentinel(expr: &Expr) -> bool {
    matches!(expr, Expr::Path(p) if p.path.is_ident("None"))
        || matches!(expr, Expr::Lit(n) if matches!(&n.lit, syn::Lit::Bool(b) if !b.value))
}

fn success_status(args: &syn::punctuated::Punctuated<Expr, syn::Token![,]>) -> bool {
    matches!(args.last(), Some(Expr::Lit(n)) if matches!(&n.lit, syn::Lit::Int(i) if i.base10_parse::<u16>().is_ok_and(|v| (100..400).contains(&v))))
}

trait ExprAttrs {
    fn attrs(&self) -> &[syn::Attribute];
}
impl ExprAttrs for Expr {
    fn attrs(&self) -> &[syn::Attribute] {
        match self {
            Expr::Call(n) => &n.attrs,
            Expr::MethodCall(n) => &n.attrs,
            Expr::Block(n) => &n.attrs,
            Expr::If(n) => &n.attrs,
            Expr::Match(n) => &n.attrs,
            Expr::Try(n) => &n.attrs,
            Expr::Assign(n) => &n.attrs,
            Expr::Macro(n) => &n.attrs,
            Expr::Return(n) => &n.attrs,
            _ => &[],
        }
    }
}

fn contains_false_ok(stream: TokenStream) -> bool {
    let trees: Vec<_> = stream.into_iter().collect();
    trees.windows(3).any(|items| {
        matches!(&items[0], TokenTree::Literal(lit) if syn::parse_str::<syn::LitStr>(&lit.to_string()).is_ok_and(|s| s.value() == "ok"))
            && matches!(&items[1], TokenTree::Punct(p) if p.as_char() == ':')
            && matches!(&items[2], TokenTree::Ident(id) if id == "false")
    }) || trees.iter().any(|tree| matches!(tree, TokenTree::Group(g) if contains_false_ok(g.stream())))
}
