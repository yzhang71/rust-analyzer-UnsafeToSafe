//! Logic for rendering the different hover messages
use std::fmt::Display;

use either::Either;
use hir::{AsAssocItem, AttributeTemplate, HasAttrs, HasSource, HirDisplay, Semantics, TypeInfo};
use ide_db::{
    base_db::SourceDatabase,
    defs::Definition,
    famous_defs::FamousDefs,
    generated::lints::{CLIPPY_LINTS, DEFAULT_LINTS, FEATURES},
    syntax_helpers::insert_whitespace_into_node,
    RootDatabase,
};

use ide_assists::{
    handlers::convert_unsafe_to_safe::{UnsafePattern, generate_safevec_format, generate_resizevec_format, generate_copywithin_format, generate_get_mut, generate_copy_from_slice_format, check_convert_type, generate_cstring_new_format}
};

use itertools::Itertools;
use stdx::format_to;
use syntax::{
    algo, ast::{self, MethodCallExpr, CallExpr, BlockExpr}, match_ast, AstNode, Direction,
    SyntaxKind::{LET_EXPR, LET_STMT, UNSAFE_KW, STMT_LIST, BIN_EXPR},
    SyntaxToken, T, SyntaxNode,
};

use crate::{
    doc_links::{remove_links, rewrite_links},
    hover::walk_and_push_ty,
    markdown_remove::remove_markdown,
    HoverAction, HoverConfig, HoverResult, Markup,
};

pub(super) fn type_info(
    sema: &Semantics<'_, RootDatabase>,
    config: &HoverConfig,
    expr_or_pat: &Either<ast::Expr, ast::Pat>,
) -> Option<HoverResult> {
    let TypeInfo { original, adjusted } = match expr_or_pat {
        Either::Left(expr) => sema.type_of_expr(expr)?,
        Either::Right(pat) => sema.type_of_pat(pat)?,
    };

    let mut res = HoverResult::default();
    let mut targets: Vec<hir::ModuleDef> = Vec::new();
    let mut push_new_def = |item: hir::ModuleDef| {
        if !targets.contains(&item) {
            targets.push(item);
        }
    };
    walk_and_push_ty(sema.db, &original, &mut push_new_def);

    res.markup = if let Some(adjusted_ty) = adjusted {
        walk_and_push_ty(sema.db, &adjusted_ty, &mut push_new_def);
        let original = original.display(sema.db).to_string();
        let adjusted = adjusted_ty.display(sema.db).to_string();
        let static_text_diff_len = "Coerced to: ".len() - "Type: ".len();
        format!(
            "{bt_start}Type: {:>apad$}\nCoerced to: {:>opad$}\n{bt_end}",
            original,
            adjusted,
            apad = static_text_diff_len + adjusted.len().max(original.len()),
            opad = original.len(),
            bt_start = if config.markdown() { "```text\n" } else { "" },
            bt_end = if config.markdown() { "```\n" } else { "" }
        )
        .into()
    } else {
        if config.markdown() {
            Markup::fenced_block(&original.display(sema.db))
        } else {
            original.display(sema.db).to_string().into()
        }
    };
    res.actions.push(HoverAction::goto_type_from_targets(sema.db, targets));
    Some(res)
}

pub(super) fn try_expr(
    sema: &Semantics<'_, RootDatabase>,
    config: &HoverConfig,
    try_expr: &ast::TryExpr,
) -> Option<HoverResult> {
    let inner_ty = sema.type_of_expr(&try_expr.expr()?)?.original;
    let mut ancestors = try_expr.syntax().ancestors();
    let mut body_ty = loop {
        let next = ancestors.next()?;
        break match_ast! {
            match next {
                ast::Fn(fn_) => sema.to_def(&fn_)?.ret_type(sema.db),
                ast::Item(__) => return None,
                ast::ClosureExpr(closure) => sema.type_of_expr(&closure.body()?)?.original,
                ast::BlockExpr(block_expr) => if matches!(block_expr.modifier(), Some(ast::BlockModifier::Async(_) | ast::BlockModifier::Try(_)| ast::BlockModifier::Const(_))) {
                    sema.type_of_expr(&block_expr.into())?.original
                } else {
                    continue;
                },
                _ => continue,
            }
        };
    };

    if inner_ty == body_ty {
        return None;
    }

    let mut inner_ty = inner_ty;
    let mut s = "Try Target".to_owned();

    let adts = inner_ty.as_adt().zip(body_ty.as_adt());
    if let Some((hir::Adt::Enum(inner), hir::Adt::Enum(body))) = adts {
        let famous_defs = FamousDefs(sema, sema.scope(try_expr.syntax())?.krate());
        // special case for two options, there is no value in showing them
        if let Some(option_enum) = famous_defs.core_option_Option() {
            if inner == option_enum && body == option_enum {
                cov_mark::hit!(hover_try_expr_opt_opt);
                return None;
            }
        }

        // special case two results to show the error variants only
        if let Some(result_enum) = famous_defs.core_result_Result() {
            if inner == result_enum && body == result_enum {
                let error_type_args =
                    inner_ty.type_arguments().nth(1).zip(body_ty.type_arguments().nth(1));
                if let Some((inner, body)) = error_type_args {
                    inner_ty = inner;
                    body_ty = body;
                    s = "Try Error".to_owned();
                }
            }
        }
    }

    let mut res = HoverResult::default();

    let mut targets: Vec<hir::ModuleDef> = Vec::new();
    let mut push_new_def = |item: hir::ModuleDef| {
        if !targets.contains(&item) {
            targets.push(item);
        }
    };
    walk_and_push_ty(sema.db, &inner_ty, &mut push_new_def);
    walk_and_push_ty(sema.db, &body_ty, &mut push_new_def);
    res.actions.push(HoverAction::goto_type_from_targets(sema.db, targets));

    let inner_ty = inner_ty.display(sema.db).to_string();
    let body_ty = body_ty.display(sema.db).to_string();
    let ty_len_max = inner_ty.len().max(body_ty.len());

    let l = "Propagated as: ".len() - " Type: ".len();
    let static_text_len_diff = l as isize - s.len() as isize;
    let tpad = static_text_len_diff.max(0) as usize;
    let ppad = static_text_len_diff.min(0).abs() as usize;

    res.markup = format!(
        "{bt_start}{} Type: {:>pad0$}\nPropagated as: {:>pad1$}\n{bt_end}",
        s,
        inner_ty,
        body_ty,
        pad0 = ty_len_max + tpad,
        pad1 = ty_len_max + ppad,
        bt_start = if config.markdown() { "```text\n" } else { "" },
        bt_end = if config.markdown() { "```\n" } else { "" }
    )
    .into();
    Some(res)
}

pub(super) fn deref_expr(
    sema: &Semantics<'_, RootDatabase>,
    config: &HoverConfig,
    deref_expr: &ast::PrefixExpr,
) -> Option<HoverResult> {
    let inner_ty = sema.type_of_expr(&deref_expr.expr()?)?.original;
    let TypeInfo { original, adjusted } =
        sema.type_of_expr(&ast::Expr::from(deref_expr.clone()))?;

    let mut res = HoverResult::default();
    let mut targets: Vec<hir::ModuleDef> = Vec::new();
    let mut push_new_def = |item: hir::ModuleDef| {
        if !targets.contains(&item) {
            targets.push(item);
        }
    };
    walk_and_push_ty(sema.db, &inner_ty, &mut push_new_def);
    walk_and_push_ty(sema.db, &original, &mut push_new_def);

    res.markup = if let Some(adjusted_ty) = adjusted {
        walk_and_push_ty(sema.db, &adjusted_ty, &mut push_new_def);
        let original = original.display(sema.db).to_string();
        let adjusted = adjusted_ty.display(sema.db).to_string();
        let inner = inner_ty.display(sema.db).to_string();
        let type_len = "To type: ".len();
        let coerced_len = "Coerced to: ".len();
        let deref_len = "Dereferenced from: ".len();
        let max_len = (original.len() + type_len)
            .max(adjusted.len() + coerced_len)
            .max(inner.len() + deref_len);
        format!(
            "{bt_start}Dereferenced from: {:>ipad$}\nTo type: {:>apad$}\nCoerced to: {:>opad$}\n{bt_end}",
            inner,
            original,
            adjusted,
            ipad = max_len - deref_len,
            apad = max_len - type_len,
            opad = max_len - coerced_len,
            bt_start = if config.markdown() { "```text\n" } else { "" },
            bt_end = if config.markdown() { "```\n" } else { "" }
        )
        .into()
    } else {
        let original = original.display(sema.db).to_string();
        let inner = inner_ty.display(sema.db).to_string();
        let type_len = "To type: ".len();
        let deref_len = "Dereferenced from: ".len();
        let max_len = (original.len() + type_len).max(inner.len() + deref_len);
        format!(
            "{bt_start}Dereferenced from: {:>ipad$}\nTo type: {:>apad$}\n{bt_end}",
            inner,
            original,
            ipad = max_len - deref_len,
            apad = max_len - type_len,
            bt_start = if config.markdown() { "```text\n" } else { "" },
            bt_end = if config.markdown() { "```\n" } else { "" }
        )
        .into()
    };
    res.actions.push(HoverAction::goto_type_from_targets(sema.db, targets));

    Some(res)
}

fn generate_description() -> String{

    return "Code Suggestion: translating unsafe to safe code".to_string();
}

fn generate_original() -> String{

    return "Original Code: \n\n".to_string();
}

fn generate_modify() -> String{

    return "Modified Code: \n\n".to_string();
}

fn format_suggestion_unitialized_vec(mcall: MethodCallExpr, unsafe_expr: &BlockExpr) -> Option<String> {

    let mut us_docs = String::new();

    let original = generate_original();

    us_docs.push_str(&original);

    let mut safe_vec = String::new();

    let mut backward_list = unsafe_expr.syntax().siblings(Direction::Prev);

    if unsafe_expr.syntax().parent()?.kind() != STMT_LIST {
        backward_list = unsafe_expr.syntax().parent()?.siblings(Direction::Prev);
    }


    for iter in backward_list {

        if iter.to_string().contains(&UnsafePattern::SetVecCapacity.to_string()) && iter.to_string().contains(&mcall.receiver()?.to_string()) {

            let let_expr = ast::LetStmt::cast(iter)?;

            format_to!(us_docs, "**```---```** **~~```{}```~~**", let_expr.to_string());
            // format_to!(us_docs, "```---``` ~~```      {}```~~", let_expr.to_string());

            us_docs.push('\n');
            us_docs.push('\n');

            format_to!(safe_vec, "**```+++```** **```{}```**", generate_safevec_format(&mcall)?.to_string());

            break;
        }

        if iter.to_string().contains(&UnsafePattern::ReserveVec.to_string()) && iter.to_string().contains(&mcall.receiver()?.to_string()) {

            let expr_stmt = ast::ExprStmt::cast(iter)?;

            format_to!(us_docs, "**```---```** **~~```{}```~~**", expr_stmt.to_string());
            // format_to!(us_docs, "```---``` ~~```      {}```~~", let_expr.to_string());

            us_docs.push('\n');
            us_docs.push('\n');

            format_to!(safe_vec, "**```+++```** **```{}```**", generate_resizevec_format(&mcall)?.to_string());

            break;
        }
    }

    us_docs.push('\n');
    us_docs.push('\n');

    let mut unsafe_vec = String::new();
    // format_to!(unsafe_vec, "```---``` ~~```      unsafe {{ {} }};```~~", mcall.to_string());
    format_to!(unsafe_vec, "**```---```** **~~```unsafe {{ {} }};```~~**", mcall.to_string());
    us_docs.push_str(&unsafe_vec);

    us_docs.push('\n');
    us_docs.push('\n');

    let modify = generate_modify();

    us_docs.push_str(&modify);

    us_docs.push_str(&safe_vec);

    return Some(us_docs.to_string());


}

fn display_suggestion_uninitialized_vec(target_expr: &SyntaxNode, unsafe_expr: &BlockExpr, actions: &Vec<HoverAction>) -> Option<HoverResult> {

    let mcall = target_expr.parent().and_then(ast::MethodCallExpr::cast)?;

    let us_description = generate_description();

    let us_docs = format_suggestion_unitialized_vec(mcall, &unsafe_expr)?;

    let markup = process_unsafe_display_text(
        &markup(Some(us_docs), us_description, None)?,
    );

    return Some(HoverResult { markup, actions: actions.to_vec() });

}

fn format_suggestion_ptr_copy(mcall: &CallExpr, unsafe_expr: &BlockExpr) -> Option<String> {

    let mut us_docs = String::new();

    let original = generate_original();

    us_docs.push_str(&original);

    format_to!(us_docs, "**```---```** **~~```unsafe {{ {} }};```~~**", mcall.to_string());

    us_docs.push('\n');
    us_docs.push('\n');

    let mut safe_copy_within = String::new();

    format_to!(safe_copy_within, "**```+++```** **```{}```**", generate_copywithin_format(&mcall, &unsafe_expr)?);

    let modify = generate_modify();

    us_docs.push_str(&modify);

    us_docs.push_str(&safe_copy_within);

    return Some(us_docs.to_string());

}

fn display_suggestion_ptr_copy(target_expr: &SyntaxNode, unsafe_expr: &BlockExpr, actions: &Vec<HoverAction>) -> Option<HoverResult> {

    let mcall = target_expr.parent().and_then(ast::CallExpr::cast)?;

    let us_description = generate_description();

    let us_docs = format_suggestion_ptr_copy(&mcall, &unsafe_expr)?;

    let markup = process_unsafe_display_text(
        &markup(Some(us_docs), us_description, None)?,
    );

    return Some(HoverResult { markup, actions: actions.to_vec() });

}

fn format_suggestion_get_uncheck_mut(mcall: MethodCallExpr) -> Option<String> {

    let mut us_docs = String::new();

    let let_expr = mcall.syntax().parent().and_then(ast::LetStmt::cast)?;

    format_to!(us_docs, "**```---```** **~~```{}```~~**", let_expr.to_string());

    us_docs.push('\n');
    us_docs.push('\n');

    let mut safe_copy_within = String::new();

    format_to!(safe_copy_within, "**```+++```** **```{}```**", generate_get_mut(&mcall, &let_expr)?);

    us_docs.push_str(&safe_copy_within);

    return Some(us_docs.to_string());

}

fn display_suggestion_get_uncheck_mut(target_expr: &SyntaxNode, actions: &Vec<HoverAction>) -> Option<HoverResult> {

    let mcall = target_expr.parent().and_then(ast::MethodCallExpr::cast)?;

    let us_description = generate_description();

    let us_docs = format_suggestion_get_uncheck_mut(mcall)?;

    let markup = process_unsafe_display_text(
        &markup(Some(us_docs), us_description, None)?,
    );

    return Some(HoverResult { markup, actions: actions.to_vec() });

}

fn format_suggestion_ptr_copy_nonoverlapping(mcall: CallExpr, unsafe_expr: &BlockExpr) -> Option<String> {

    let mut us_docs = String::new();

    format_to!(us_docs, "**```---```** **~~```unsafe {{ {} }};```~~**", mcall.to_string());

    us_docs.push('\n');
    us_docs.push('\n');

    let mut safe_copy_within = String::new();

    format_to!(safe_copy_within, "**```+++```** **```{}```**", generate_copy_from_slice_format(&mcall, &unsafe_expr)?);

    us_docs.push_str(&safe_copy_within);

    return Some(us_docs.to_string());

}

fn display_suggestion_ptr_copy_nonoverlapping(target_expr: &SyntaxNode, unsafe_expr: &BlockExpr, actions: &Vec<HoverAction>) -> Option<HoverResult> {

    let mcall = target_expr.parent().and_then(ast::CallExpr::cast)?;

    let us_description = generate_description();

    let us_docs = format_suggestion_ptr_copy_nonoverlapping(mcall, &unsafe_expr)?;

    let markup = process_unsafe_display_text(
        &markup(Some(us_docs), us_description, None)?,
    );

    return Some(HoverResult { markup, actions: actions.to_vec() });

}

fn format_suggestion_cstring_from_vec_unchecked(mcall: CallExpr) -> Option<String> {

    let mut us_docs = String::new();

    if mcall.syntax().parent()?.kind() == BIN_EXPR {

        let target_expr = mcall.syntax().parent().and_then(ast::BinExpr::cast)?;

        format_to!(us_docs, "**```---```** **~~```unsafe {{ {} }}```~~**", target_expr.to_string());
    
        us_docs.push('\n');
        us_docs.push('\n');
    
        let mut safe_cstring_new = String::new();
    
        format_to!(safe_cstring_new, "**```+++```** **```{}```**", generate_cstring_new_format(target_expr.lhs()?.to_string(), &mcall, false)?);
        
        us_docs.push_str(&safe_cstring_new);
    
        return Some(us_docs.to_string());
    }

    let let_expr = mcall.syntax().parent().and_then(ast::LetStmt::cast)?;

    format_to!(us_docs, "**```---```** **~~```unsafe {{ {} }}```~~**", let_expr.to_string());

    us_docs.push('\n');
    us_docs.push('\n');

    let mut safe_cstring_new = String::new();

    format_to!(safe_cstring_new, "**```+++```** **```{}```**", generate_cstring_new_format(let_expr.pat()?.to_string(), &mcall, true)?);

    us_docs.push_str(&safe_cstring_new);

    return Some(us_docs.to_string());

}


fn display_suggestion_cstring_from_vec_unchecked(target_expr: &SyntaxNode, actions: &Vec<HoverAction>) -> Option<HoverResult> {

    let mcall = target_expr.parent().and_then(ast::CallExpr::cast)?;

    let us_description = generate_description();

    let us_docs = format_suggestion_cstring_from_vec_unchecked(mcall)?;

    let markup = process_unsafe_display_text(
        &markup(Some(us_docs), us_description, None)?,
    );

    return Some(HoverResult { markup, actions: actions.to_vec() });

}

fn format_suggestion_cstring_bytes_len(mcall: CallExpr) -> Option<String> {

    let mut us_docs = String::new();

    if mcall.syntax().parent()?.kind() == BIN_EXPR {

        let target_expr = mcall.syntax().parent().and_then(ast::BinExpr::cast)?;

        format_to!(us_docs, "**```---```** **~~```unsafe {{ {} }}```~~**", target_expr.to_string());
    
        us_docs.push('\n');
        us_docs.push('\n');
    
        let mut safe_cstring_new = String::new();
    
        format_to!(safe_cstring_new, "**```+++```** **```{}```**", generate_bytes_len_format(target_expr.lhs()?.to_string(), &mcall, false)?);
        
        us_docs.push_str(&safe_cstring_new);
    
        return Some(us_docs.to_string());
    }

    let let_expr = mcall.syntax().parent().and_then(ast::LetStmt::cast)?;

    format_to!(us_docs, "**```---```** **~~```unsafe {{ {} }}```~~**", let_expr.to_string());

    us_docs.push('\n');
    us_docs.push('\n');

    let mut safe_cstring_new = String::new();

    format_to!(safe_cstring_new, "**```+++```** **```{}```**", generate_bytes_len_format(let_expr.pat()?.to_string(), &mcall, true)?);

    us_docs.push_str(&safe_cstring_new);

    return Some(us_docs.to_string());

}

fn display_suggestion_cstring_bytes_len(target_expr: &SyntaxNode, actions: &Vec<HoverAction>) -> Option<HoverResult> {

    let mcall = target_expr.parent().and_then(ast::CallExpr::cast)?;

    let us_description = generate_description();

    let us_docs = format_suggestion_cstring_bytes_len(mcall)?;

    let markup = process_unsafe_display_text(
        &markup(Some(us_docs), us_description, None)?,
    );

    return Some(HoverResult { markup, actions: actions.to_vec() });

}



pub(super) fn keyword(
    sema: &Semantics<'_, RootDatabase>,
    config: &HoverConfig,
    token: &SyntaxToken,
) -> Option<HoverResult> {
    if !token.kind().is_keyword() || !config.documentation.is_some() || !config.keywords {
        return None;
    }

    let parent = token.parent()?;
    let famous_defs = FamousDefs(sema, sema.scope(&parent)?.krate());

    let KeywordHint { description, keyword_mod, actions } = keyword_hints(sema, token, parent);
    
    // Yuchen's Edit -> Detect unsafe keyword
    if token.kind() == UNSAFE_KW {

        let unsafe_expr = token.parent().and_then(ast::BlockExpr::cast)?;

        for target_expr in unsafe_expr.syntax().descendants() {

            let unsafe_type = check_convert_type(&target_expr, &unsafe_expr);

            match unsafe_type {
                Some(UnsafePattern::UnitializedVec) => return display_suggestion_uninitialized_vec(&target_expr, &unsafe_expr, &actions),
                Some(UnsafePattern::CopyWithin) => return display_suggestion_ptr_copy(&target_expr, &unsafe_expr, &actions),
                Some(UnsafePattern::CopyNonOverlap) => return display_suggestion_ptr_copy_nonoverlapping(&target_expr, &unsafe_expr, &actions),
                Some(UnsafePattern::CStringFromVec) => return display_suggestion_cstring_from_vec_unchecked(&target_expr, &actions),
                Some(UnsafePattern::CStringLength) => return display_suggestion_cstring_bytes_len(&target_expr, &actions),
                // Some(UnsafePattern::GetUncheckMut) => return display_suggestion_get_uncheck_mut(&target_expr, &actions),
                // Some(UnsafePattern::GetUncheck) => return display_suggestion_get_uncheck_mut(&target_expr, &actions),
                None => continue,
                _ => todo!(),
            };
        }
    }

    let doc_owner = find_std_module(&famous_defs, &keyword_mod)?;
    let docs = doc_owner.attrs(sema.db).docs()?;
    let markup = process_markup(
        sema.db,
        Definition::Module(doc_owner),
        &markup(Some(docs.into()), description, None)?,
        config,
    );
    return Some(HoverResult { markup, actions });

}

pub(super) fn try_for_lint(attr: &ast::Attr, token: &SyntaxToken) -> Option<HoverResult> {
    let (path, tt) = attr.as_simple_call()?;
    if !tt.syntax().text_range().contains(token.text_range().start()) {
        return None;
    }
    let (is_clippy, lints) = match &*path {
        "feature" => (false, FEATURES),
        "allow" | "deny" | "forbid" | "warn" => {
            let is_clippy = algo::non_trivia_sibling(token.clone().into(), Direction::Prev)
                .filter(|t| t.kind() == T![:])
                .and_then(|t| algo::non_trivia_sibling(t, Direction::Prev))
                .filter(|t| t.kind() == T![:])
                .and_then(|t| algo::non_trivia_sibling(t, Direction::Prev))
                .map_or(false, |t| {
                    t.kind() == T![ident] && t.into_token().map_or(false, |t| t.text() == "clippy")
                });
            if is_clippy {
                (true, CLIPPY_LINTS)
            } else {
                (false, DEFAULT_LINTS)
            }
        }
        _ => return None,
    };

    let tmp;
    let needle = if is_clippy {
        tmp = format!("clippy::{}", token.text());
        &tmp
    } else {
        &*token.text()
    };

    let lint =
        lints.binary_search_by_key(&needle, |lint| lint.label).ok().map(|idx| &lints[idx])?;
    Some(HoverResult {
        markup: Markup::from(format!("```\n{}\n```\n___\n\n{}", lint.label, lint.description)),
        ..Default::default()
    })
}

pub(super) fn process_markup(
    db: &RootDatabase,
    def: Definition,
    markup: &Markup,
    config: &HoverConfig,
) -> Markup {
    let markup = markup.as_str();
    let markup = if !config.markdown() {
        remove_markdown(markup)
    } else if config.links_in_hover {
        rewrite_links(db, markup, def)
    } else {
        remove_links(markup)
    };
    Markup::from(markup)
}

pub(super) fn process_unsafe_display_text(
    markup: &Markup,
) -> Markup {
    let markup = markup.as_str();
    let markup = markup.to_string();
    Markup::from(markup)
}

fn definition_owner_name(db: &RootDatabase, def: &Definition) -> Option<String> {
    match def {
        Definition::Field(f) => Some(f.parent_def(db).name(db)),
        Definition::Local(l) => l.parent(db).name(db),
        Definition::Function(f) => match f.as_assoc_item(db)?.container(db) {
            hir::AssocItemContainer::Trait(t) => Some(t.name(db)),
            hir::AssocItemContainer::Impl(i) => i.self_ty(db).as_adt().map(|adt| adt.name(db)),
        },
        Definition::Variant(e) => Some(e.parent_enum(db).name(db)),
        _ => None,
    }
    .map(|name| name.to_string())
}

pub(super) fn path(db: &RootDatabase, module: hir::Module, item_name: Option<String>) -> String {
    let crate_name =
        db.crate_graph()[module.krate().into()].display_name.as_ref().map(|it| it.to_string());
    let module_path = module
        .path_to_root(db)
        .into_iter()
        .rev()
        .flat_map(|it| it.name(db).map(|name| name.to_string()));
    crate_name.into_iter().chain(module_path).chain(item_name).join("::")
}

pub(super) fn definition(
    db: &RootDatabase,
    def: Definition,
    famous_defs: Option<&FamousDefs<'_, '_>>,
    config: &HoverConfig,
) -> Option<Markup> {
    let mod_path = definition_mod_path(db, &def);
    let (label, docs) = match def {
        Definition::Macro(it) => label_and_docs(db, it),
        Definition::Field(it) => label_and_docs(db, it),
        Definition::Module(it) => label_and_docs(db, it),
        Definition::Function(it) => label_and_docs(db, it),
        Definition::Adt(it) => label_and_docs(db, it),
        Definition::Variant(it) => label_value_and_docs(db, it, |&it| {
            if !it.parent_enum(db).is_data_carrying(db) {
                match it.eval(db) {
                    Ok(x) => Some(format!("{}", x)),
                    Err(_) => it.value(db).map(|x| format!("{:?}", x)),
                }
            } else {
                None
            }
        }),
        Definition::Const(it) => label_value_and_docs(db, it, |it| {
            let body = it.eval(db);
            match body {
                Ok(x) => Some(format!("{}", x)),
                Err(_) => {
                    let source = it.source(db)?;
                    let mut body = source.value.body()?.syntax().clone();
                    if source.file_id.is_macro() {
                        body = insert_whitespace_into_node::insert_ws_into(body);
                    }
                    Some(body.to_string())
                }
            }
        }),
        Definition::Static(it) => label_value_and_docs(db, it, |it| {
            let source = it.source(db)?;
            let mut body = source.value.body()?.syntax().clone();
            if source.file_id.is_macro() {
                body = insert_whitespace_into_node::insert_ws_into(body);
            }
            Some(body.to_string())
        }),
        Definition::Trait(it) => label_and_docs(db, it),
        Definition::TypeAlias(it) => label_and_docs(db, it),
        Definition::BuiltinType(it) => {
            return famous_defs
                .and_then(|fd| builtin(fd, it))
                .or_else(|| Some(Markup::fenced_block(&it.name())))
        }
        Definition::Local(it) => return local(db, it),
        Definition::SelfType(impl_def) => {
            impl_def.self_ty(db).as_adt().map(|adt| label_and_docs(db, adt))?
        }
        Definition::GenericParam(it) => label_and_docs(db, it),
        Definition::Label(it) => return Some(Markup::fenced_block(&it.name(db))),
        // FIXME: We should be able to show more info about these
        Definition::BuiltinAttr(it) => return render_builtin_attr(db, it),
        Definition::ToolModule(it) => return Some(Markup::fenced_block(&it.name(db))),
        Definition::DeriveHelper(it) => (format!("derive_helper {}", it.name(db)), None),
    };

    let docs = match config.documentation {
        Some(_) => docs.or_else(|| {
            // docs are missing, for assoc items of trait impls try to fall back to the docs of the
            // original item of the trait
            let assoc = def.as_assoc_item(db)?;
            let trait_ = assoc.containing_trait_impl(db)?;
            let name = Some(assoc.name(db)?);
            let item = trait_.items(db).into_iter().find(|it| it.name(db) == name)?;
            item.docs(db)
        }),
        None => None,
    };
    let docs = docs.filter(|_| config.documentation.is_some()).map(Into::into);
    markup(docs, label, mod_path)
}

fn render_builtin_attr(db: &RootDatabase, attr: hir::BuiltinAttr) -> Option<Markup> {
    let name = attr.name(db);
    let desc = format!("#[{}]", name);

    let AttributeTemplate { word, list, name_value_str } = match attr.template(db) {
        Some(template) => template,
        None => return Some(Markup::fenced_block(&attr.name(db))),
    };
    let mut docs = "Valid forms are:".to_owned();
    if word {
        format_to!(docs, "\n - #\\[{}]", name);
    }
    if let Some(list) = list {
        format_to!(docs, "\n - #\\[{}({})]", name, list);
    }
    if let Some(name_value_str) = name_value_str {
        format_to!(docs, "\n - #\\[{} = {}]", name, name_value_str);
    }
    markup(Some(docs.replace('*', "\\*")), desc, None)
}

fn label_and_docs<D>(db: &RootDatabase, def: D) -> (String, Option<hir::Documentation>)
where
    D: HasAttrs + HirDisplay,
{
    let label = def.display(db).to_string();
    let docs = def.attrs(db).docs();
    (label, docs)
}

fn label_value_and_docs<D, E, V>(
    db: &RootDatabase,
    def: D,
    value_extractor: E,
) -> (String, Option<hir::Documentation>)
where
    D: HasAttrs + HirDisplay,
    E: Fn(&D) -> Option<V>,
    V: Display,
{
    let label = if let Some(value) = value_extractor(&def) {
        format!("{} = {}", def.display(db), value)
    } else {
        def.display(db).to_string()
    };
    let docs = def.attrs(db).docs();
    (label, docs)
}

fn definition_mod_path(db: &RootDatabase, def: &Definition) -> Option<String> {
    if let Definition::GenericParam(_) = def {
        return None;
    }
    def.module(db).map(|module| path(db, module, definition_owner_name(db, def)))
}

fn markup(docs: Option<String>, desc: String, mod_path: Option<String>) -> Option<Markup> {
    let mut buf = String::new();

    if let Some(mod_path) = mod_path {
        if !mod_path.is_empty() {
            format_to!(buf, "```rust\n{}\n```\n\n", mod_path);
        }
    }
    format_to!(buf, "```rust\n{}\n```", desc);

    if let Some(doc) = docs {
        format_to!(buf, "\n___\n\n{}", doc);
    }
    Some(buf.into())
}

fn builtin(famous_defs: &FamousDefs<'_, '_>, builtin: hir::BuiltinType) -> Option<Markup> {
    // std exposes prim_{} modules with docstrings on the root to document the builtins
    let primitive_mod = format!("prim_{}", builtin.name());
    let doc_owner = find_std_module(famous_defs, &primitive_mod)?;
    let docs = doc_owner.attrs(famous_defs.0.db).docs()?;
    markup(Some(docs.into()), builtin.name().to_string(), None)
}

fn find_std_module(famous_defs: &FamousDefs<'_, '_>, name: &str) -> Option<hir::Module> {
    let db = famous_defs.0.db;
    let std_crate = famous_defs.std()?;
    let std_root_module = std_crate.root_module(db);
    std_root_module
        .children(db)
        .find(|module| module.name(db).map_or(false, |module| module.to_string() == name))
}

fn local(db: &RootDatabase, it: hir::Local) -> Option<Markup> {
    let ty = it.ty(db);
    let ty = ty.display_truncated(db, None);
    let is_mut = if it.is_mut(db) { "mut " } else { "" };
    let desc = match it.source(db).value {
        Either::Left(ident) => {
            let name = it.name(db);
            let let_kw = if ident
                .syntax()
                .parent()
                .map_or(false, |p| p.kind() == LET_STMT || p.kind() == LET_EXPR)
            {
                "let "
            } else {
                ""
            };
            format!("{}{}{}: {}", let_kw, is_mut, name, ty)
        }
        Either::Right(_) => format!("{}self: {}", is_mut, ty),
    };
    markup(None, desc, None)
}

struct KeywordHint {
    description: String,
    keyword_mod: String,
    actions: Vec<HoverAction>,
}

impl KeywordHint {
    fn new(description: String, keyword_mod: String) -> Self {
        Self { description, keyword_mod, actions: Vec::default() }
    }
}

fn keyword_hints(
    sema: &Semantics<'_, RootDatabase>,
    token: &SyntaxToken,
    parent: syntax::SyntaxNode,
) -> KeywordHint {
    match token.kind() {
        T![await] | T![loop] | T![match] | T![unsafe] | T![as] | T![try] | T![if] | T![else] => {
            let keyword_mod = format!("{}_keyword", token.text());

            match ast::Expr::cast(parent).and_then(|site| sema.type_of_expr(&site)) {
                // ignore the unit type ()
                Some(ty) if !ty.adjusted.as_ref().unwrap_or(&ty.original).is_unit() => {
                    let mut targets: Vec<hir::ModuleDef> = Vec::new();
                    let mut push_new_def = |item: hir::ModuleDef| {
                        if !targets.contains(&item) {
                            targets.push(item);
                        }
                    };
                    walk_and_push_ty(sema.db, &ty.original, &mut push_new_def);

                    let ty = ty.adjusted();
                    let description = format!("{}: {}", token.text(), ty.display(sema.db));

                    KeywordHint {
                        description,
                        keyword_mod,
                        actions: vec![HoverAction::goto_type_from_targets(sema.db, targets)],
                    }
                }
                _ => KeywordHint {
                    description: token.text().to_string(),
                    keyword_mod,
                    actions: Vec::new(),
                },
            }
        }
        T![fn] => {
            let module = match ast::FnPtrType::cast(parent) {
                // treat fn keyword inside function pointer type as primitive
                Some(_) => format!("prim_{}", token.text()),
                None => format!("{}_keyword", token.text()),
            };
            KeywordHint::new(token.text().to_string(), module)
        }
        T![Self] => KeywordHint::new(token.text().to_string(), "self_upper_keyword".into()),
        _ => KeywordHint::new(token.text().to_string(), format!("{}_keyword", token.text())),
    }
}
