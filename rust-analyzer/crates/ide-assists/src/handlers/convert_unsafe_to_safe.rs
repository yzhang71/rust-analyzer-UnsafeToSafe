use crate::{
    assist_context::{AssistContext, Assists},
    AssistId, AssistKind,
};

use syntax::{ast::{IndexExpr, BlockExpr, MethodCallExpr, ExprStmt, CallExpr, edit_in_place::Indent, LetStmt}, TextSize, Direction};
use itertools::Itertools;
use stdx::format_to;
use syntax::{
    ast::{
        self,
        AstNode,
        HasArgList,
    },
    SyntaxNode, TextRange, T,
};

// Assist: convert_while_to_loop
//
// Replace unsafe code with safe version.
//
// ```
// fn main() {
//
//     let mut buffer = Vec::with_capacity(cap);
//
//     unsafe {
//         buffer.set_len(cap); 
//         foo();
//     }
// }
// ```
// ->
// ```
// fn main() {
//    
//     let mut buffer = vec![0; cap];
//  
//     unsafe {
//         foo();
//     }
//     
// }
// ```

pub enum UnsafePattern {
    SetVecCapacity,
    UnitializedVec,
    CopyWithin,
    GetUncheck,
    GetUncheckMut,
    CopyNonOverlap,
}

impl std::fmt::Display for UnsafePattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnsafePattern::SetVecCapacity => write!(f, "Vec::with_capacity"),
            UnsafePattern::UnitializedVec => write!(f, "set_len"),
            UnsafePattern::CopyWithin => write!(f, "ptr::copy"),
            UnsafePattern::GetUncheck => write!(f, "get_unchecked"),
            UnsafePattern::GetUncheckMut => write!(f, "get_unchecked_mut"),
            UnsafePattern::CopyNonOverlap => write!(f, "ptr::copy_nonoverlapping"),
        }
    }
}

pub fn generate_safevec_format(mcall: &MethodCallExpr) -> Option<String> {

    // Obtain the variable Expr that presents the buffer/vector
    let receiver = mcall.receiver()?;

    let closure_body = mcall.arg_list()?.args().exactly_one().ok()?;

    let mut buf = String::new();

    format_to!(buf, "let mut {} = vec![0; {}];", receiver, closure_body);

    return Some(buf);

}

fn check_single_expr(target_expr: &ExprStmt) -> bool {

    // Check if the unsafe bloack only contains one expr
    if target_expr.syntax().prev_sibling().is_none() && target_expr.syntax().next_sibling().is_none() {
        return true;
    }
    return false;
}

fn delet_replace_source_code(acc: &mut Assists, let_target: TextRange, target_range: TextRange, buf: &String) {

    acc.add(
        AssistId("convert_unsafe_to_safe", AssistKind::RefactorRewrite),
        "Convert Unsafe to Safe",
        target_range,
        |edit| {
            edit.delete(target_range);
            edit.replace(let_target, buf)
        },
    );
}

fn convert_to_auto_vec_initialization(acc: &mut Assists, target_expr: &SyntaxNode, unsafe_range: TextRange, unsafe_expr: &BlockExpr) -> Option<()> {

    let mcall = target_expr.parent().and_then(ast::MethodCallExpr::cast)?;

    let buf = if let Some(buffer) = generate_safevec_format(&mcall) {buffer} else { return None; };

    // Declare the target text range for modification.
    let target_expr = mcall.syntax().parent().and_then(ast::ExprStmt::cast)?;

    let mut target_range = target_expr.syntax().text_range();
    if check_single_expr(&target_expr) {
        target_range = unsafe_range;
    }

    for iter in unsafe_expr.syntax().parent()?.siblings(Direction::Prev) {

        if iter.to_string().contains(&UnsafePattern::SetVecCapacity.to_string()) {

            let let_expr = ast::LetStmt::cast(iter)?;
                
            let let_target = let_expr.syntax().text_range();
            // Delete the "set_len" expression in unsafe code block and insert the auto initialized vec/buf
            delet_replace_source_code(acc, let_target, target_range, &buf);

            return None;

        }
    }

    return None;
}

pub fn generate_copywithin_string(base_expr: String, start_pos: String, end_pos: String, count_expr: String) -> String {

    let mut buf = String::new();

    format_to!(buf, "{}.copy_within({}..{}, {});", base_expr, start_pos, end_pos, count_expr);

    buf.push('\n');

    return buf;

}

struct CpyWithinInfo {
    base_expr: String,
    start_pos: String,
    end_pos: String,
    count_expr: String,
}

fn collect_cpy_within_info(mcall: &CallExpr, src_expr: IndexExpr, dst_expr: IndexExpr) -> Option<CpyWithinInfo> {

    let count_expr = mcall.arg_list()?.args().nth(2)?.to_string();

    let base_expr = src_expr.base()?.to_string();

    let start_pos = src_expr.index()?.to_string();

    let end_pos = dst_expr.index()?.to_string();

    return Some(CpyWithinInfo {base_expr, start_pos, end_pos, count_expr});
}

struct PtrCpyInfo {
    src_expr: IndexExpr,
    dst_expr: IndexExpr,
}

fn collect_ptr_cpy_info(mcall: &CallExpr) -> Option<PtrCpyInfo> {

    let src_expr = ast::IndexExpr::cast(mcall.arg_list()?.args().nth(0)?.syntax().children().nth(0)?.children().nth(0)?)?;

    let dst_expr = ast::IndexExpr::cast(mcall.arg_list()?.args().nth(1)?.syntax().children().nth(0)?.children().nth(0)?)?;

    return Some(PtrCpyInfo {src_expr, dst_expr});
}

fn delet_insert_source_code(acc: &mut Assists, target_range: TextRange, position: TextSize, new_buf: &String) {

    acc.add(
        AssistId("convert_unsafe_to_safe", AssistKind::RefactorRewrite),
        "Convert Unsafe to Safe",
        target_range,
        |edit| {
            edit.delete(target_range);
            edit.insert(position + TextSize::of('\n'), new_buf)
        },
    );
}

pub fn generate_copywithin_format(mcall: &CallExpr) -> Option<String> {

    let PtrCpyInfo { src_expr, dst_expr} = collect_ptr_cpy_info(&mcall)?;

    let CpyWithinInfo { base_expr, start_pos, end_pos, count_expr} = collect_cpy_within_info(&mcall, src_expr, dst_expr)?;

    let buf = generate_copywithin_string(base_expr, start_pos, end_pos, count_expr);

    return Some(buf);

}

fn replace_source_code(acc: &mut Assists, target_range: TextRange, buf: &String) {
    acc.add(
        AssistId("convert_unsafe_to_safe", AssistKind::RefactorRewrite),
        "Convert Unsafe to Safe",
        target_range,
        |edit| {
            edit.replace(target_range, buf)
        },
    );
}

fn reindent_expr(unsafe_expr: &BlockExpr, acc: &mut Assists, target_range: TextRange, buf: &String) -> Option<()> {

    let position = unsafe_expr.syntax().prev_sibling()?.text_range().end();

    let indent_level = unsafe_expr.indent_level();

    let mut new_buf = String::new();

    format_to!(new_buf, "{}{}", indent_level, buf);

    delet_insert_source_code(acc, target_range, position, &new_buf);

    return None;

}

fn convert_to_copy_within(acc: &mut Assists, target_expr: &SyntaxNode, unsafe_range: TextRange, unsafe_expr: &BlockExpr) -> Option<()> {

    let mcall = target_expr.parent().and_then(ast::CallExpr::cast)?;

    let target_expr = mcall.syntax().parent().and_then(ast::ExprStmt::cast)?;

    let mut target_range = target_expr.syntax().text_range();

    let buf = generate_copywithin_format(&mcall)?;

    if check_single_expr(&target_expr) {
        target_range = unsafe_range;
        replace_source_code(acc, target_range, &buf);
        return None;
    }

    return reindent_expr(unsafe_expr, acc, target_range, &buf);
}

pub fn generate_get_mut(mcall: &MethodCallExpr, let_expr: &LetStmt) -> Option<String> {

    // Obtain the variable Expr that presents the buffer/vector
    let receiver = mcall.receiver()?;

    let closure_body = mcall.arg_list()?.args().exactly_one().ok()?;

    let pat = let_expr.pat()?;

    let mut buf = String::new();

    if let_expr.initializer()?.to_string().contains("mut") {
        format_to!(buf, "let {} = {}.get_mut({});", pat, receiver, closure_body);
    } else {
        format_to!(buf, "let {} = {}.get({});", pat, receiver, closure_body);
    }

    return Some(buf);

}

fn check_single_let_expr(target_expr: &LetStmt) -> bool {

    // Check if the unsafe bloack only contains one expr
    if target_expr.syntax().prev_sibling().is_none() && target_expr.syntax().next_sibling().is_none() {
        return true;
    }
    return false;
}

fn convert_to_get_mut(acc: &mut Assists, target_expr: &SyntaxNode, unsafe_range: TextRange, unsafe_expr: &BlockExpr) -> Option<()> {

    let mcall = target_expr.parent().and_then(ast::MethodCallExpr::cast)?;

    let let_expr = mcall.syntax().parent().and_then(ast::LetStmt::cast)?;

    let buf = generate_get_mut(&mcall, &let_expr)?;

    let mut target_range = let_expr.syntax().text_range();
    if check_single_let_expr(&let_expr) {
        target_range = unsafe_range;
        replace_source_code(acc, target_range, &buf);
        return None;
    }

    return reindent_expr(unsafe_expr, acc, target_range, &buf);

}

struct CpyNonOverlapInfo {
    src_expr: IndexExpr,
    dst_expr: IndexExpr,
}

fn collect_cpy_nonoverlap_info(mcall: &CallExpr) -> Option<CpyNonOverlapInfo> {

    let src_expr = ast::IndexExpr::cast(mcall.arg_list()?.args().nth(0)?.syntax().children().nth(0)?)?;

    let dst_expr = ast::IndexExpr::cast(mcall.arg_list()?.args().nth(1)?.syntax().children().nth(0)?)?;

    return Some(CpyNonOverlapInfo {src_expr, dst_expr});
}

pub fn generate_copy_from_slice_string(src_expr: IndexExpr, dst_expr: IndexExpr) -> String {

    let mut buf = String::new();

    format_to!(buf, "{}.copy_from_slice(&{});", dst_expr, src_expr);

    buf.push('\n');

    return buf;

}

pub fn generate_copy_from_slice_format(mcall: &CallExpr) -> Option<String> {    

    let CpyNonOverlapInfo { src_expr, dst_expr} = collect_cpy_nonoverlap_info(&mcall)?;

    let buf = generate_copy_from_slice_string(src_expr, dst_expr);

    return Some(buf);
}

fn convert_to_copy_from_slice(acc: &mut Assists, target_expr: &SyntaxNode, unsafe_range: TextRange, unsafe_expr: &BlockExpr) -> Option<()> {

    let mcall = target_expr.parent().and_then(ast::CallExpr::cast)?;

    let target_expr = mcall.syntax().parent().and_then(ast::ExprStmt::cast)?;

    let mut target_range = target_expr.syntax().text_range();

    let buf = generate_copy_from_slice_format(&mcall)?;

    if check_single_expr(&target_expr) {
        target_range = unsafe_range;
        replace_source_code(acc, target_range, &buf);
        return None;
    }

    return reindent_expr(unsafe_expr, acc, target_range, &buf);

}

pub fn check_convert_type(target_expr: &SyntaxNode, unsafe_expr: &BlockExpr) -> Option<UnsafePattern> {

    if target_expr.to_string() == UnsafePattern::UnitializedVec.to_string() {
        for backward_slice in unsafe_expr.syntax().parent()?.siblings(Direction::Prev) {
            if backward_slice.to_string().contains(&UnsafePattern::SetVecCapacity.to_string()) {
                for forward_slice in unsafe_expr.syntax().parent()?.siblings(Direction::Next) {
                    if forward_slice.to_string().contains("read") {
                        return Some(UnsafePattern::UnitializedVec);
                    }
                }
            }
        }
    }

    if target_expr.to_string() == UnsafePattern::CopyWithin.to_string() {
        return Some(UnsafePattern::CopyWithin);
    }

    if target_expr.to_string() == UnsafePattern::GetUncheck.to_string() {
        return Some(UnsafePattern::GetUncheck);
    }

    if target_expr.to_string() == UnsafePattern::GetUncheckMut.to_string() {
        return Some(UnsafePattern::GetUncheckMut);
    }

    if target_expr.to_string() == UnsafePattern::CopyNonOverlap.to_string() {
        return Some(UnsafePattern::CopyNonOverlap);
    }
    return None;

}

struct UnsafeBlockInfo {
    unsafe_expr: BlockExpr,
    unsafe_range: TextRange,
}

fn collect_unsafe_vec_info(ctx: &AssistContext<'_>) -> Option<UnsafeBlockInfo> {

    // Detect the "unsafe" key word
    let unsafe_kw = ctx.find_token_syntax_at_offset(T![unsafe])?;

    // Collect the expressions within the "unsafe" block
    let unsafe_expr = unsafe_kw.parent().and_then(ast::BlockExpr::cast)?;

    let unsafe_range = unsafe_expr.syntax().text_range();

    return Some(UnsafeBlockInfo {unsafe_expr, unsafe_range});

}

pub(crate) fn convert_unsafe_to_safe(acc: &mut Assists, ctx: &AssistContext<'_>) -> Option<()> {

    let UnsafeBlockInfo { unsafe_expr, unsafe_range} = collect_unsafe_vec_info(ctx)?;

    // Iteration through the "unsafe" expressions' AST
    for target_expr in unsafe_expr.syntax().descendants() {

        let unsafe_type = check_convert_type(&target_expr, &unsafe_expr);
        
        match unsafe_type {
            Some(UnsafePattern::UnitializedVec) => convert_to_auto_vec_initialization(acc, &target_expr, unsafe_range, &unsafe_expr),
            // Some(UnsafePattern::CopyWithin) => convert_to_copy_within(acc, &target_expr, unsafe_range, &unsafe_expr),
            // Some(UnsafePattern::GetUncheckMut) => convert_to_get_mut(acc, &target_expr, unsafe_range, &unsafe_expr),
            // Some(UnsafePattern::GetUncheck) => convert_to_get_mut(acc, &target_expr, unsafe_range, &unsafe_expr),
            // Some(UnsafePattern::CopyNonOverlap) => convert_to_copy_from_slice(acc, &target_expr, unsafe_range, &unsafe_expr),
            None => continue,
            _ => todo!(),
        };
        
    }

    return None;
    
}

#[cfg(test)]
mod tests {
    use crate::tests::check_assist;

    use super::*;

    #[test]
    fn copy_nonoverlap_1() {
        check_assist(
            convert_unsafe_to_safe,
            r#"
    fn main() {

        let src = vec![1, 2, 3, 4, 5, 6];
        let mut dst = vec![0; 6];
    
        unsafe$0 {
            ptr::copy_nonoverlapping(src[2..4].as_ptr(), dst[2..4].as_mut_ptr(), src[2..4].len());
            println!("copied dst vector: {:?}", dst); 
        }
    }
    "#,
                r#"
    fn main() {

        let src = vec![1, 2, 3, 4, 5, 6];
        let mut dst = vec![0; 6];
        dst[2..4].copy_from_slice(&src[2..4]);
    
        unsafe$0 {

            println!("copied dst vector: {:?}", dst); 
        }
    }
    "#,
            );
    }

    #[test]
    fn copy_nonoverlap_2() {
        check_assist(
            convert_unsafe_to_safe,
            r#"
    fn main() {

        let src = vec![1, 2, 3, 4, 5, 6];
        let mut dst = vec![0; 6];
    
        unsafe$0 {
            ptr::copy_nonoverlapping(src[2..4].as_ptr(), dst[2..4].as_mut_ptr(), src[2..4].len());
        }
    }
    "#,
                r#"
    fn main() {

        let src = vec![1, 2, 3, 4, 5, 6];
        let mut dst = vec![0; 6];

        dst[2..4].copy_from_slice(&src[2..4]);

    }
    "#,
            );
    }

    #[test]
    fn get_uncheckd_1() {
        check_assist(
            convert_unsafe_to_safe,
            r#"
    fn main() {

        let mut vec = vec![1,2,3,4,5,6];
    
        unsafe$0 {
            let index = vec.get_unchecked_mut(5);    
            print!("Index: {:?} \n", index);
        }
    }
    "#,
                r#"
    fn main() {

        let mut vec = vec![1,2,3,4,5,6];
        let index = vec.get_mut(5);
        unsafe$0 {

            print!("Index: {:?} \n", index);
        }
    }
    "#,
            );
    }

    #[test]
    fn get_uncheckd_2() {
        check_assist(
            convert_unsafe_to_safe,
            r#"
    fn main() {

        let mut vec = vec![1,2,3,4,5,6];
    
        unsafe$0 {
            let index = vec.get_unchecked_mut(5);    
        }
    }
    "#,
                r#"
    fn main() {

        let mut vec = vec![1,2,3,4,5,6];

        let index = vec.get_mut(5);
    }
    "#,
            );
    }

    #[test]
    fn convert_ptr_copy_1() {
        check_assist(
            convert_unsafe_to_safe,
            r#"
    fn main() {

        let mut vec = vec![1,2,3,4,5,6];
    
        unsafe$0 {
            ptr::copy(&vec[0] as *const i32, &mut vec[3] as *mut i32, 3);
        }
    }
    "#,
                r#"
    fn main() {

        let mut vec = vec![1,2,3,4,5,6];

        vec.copy_within(0..3, 3);

    }
    "#,
            );
    }

    #[test]
    fn convert_ptr_copy_2() {
        check_assist(
            convert_unsafe_to_safe,
            r#"
    fn main() {

        let mut vec = vec![1,2,3,4,5,6];
    
        unsafe$0 {
            ptr::copy(&vec[0] as *const i32, &mut vec[3] as *mut i32, 3);
            println!("Hello World!");
        }
    }
    "#,
                r#"
    fn main() {

        let mut vec = vec![1,2,3,4,5,6];
        vec.copy_within(0..3, 3);

        unsafe$0 {
            
            println!("Hello World!");
        }
    }
    "#,
            );
    }

    #[test]
    fn convert_vec_1() {
        check_assist(
            convert_unsafe_to_safe,
            r#"
    fn main() {

        let cap = 100;

        let mut buffer = Vec::with_capacity(cap);

        unsafe$0 {
            buffer.set_len(cap); 
            println!("Hello World!");
        }
    }
    "#,
                r#"
    fn main() {

        let cap = 100;

        let mut buffer = vec![0; cap];

        unsafe$0 {
            
            println!("Hello World!");
        }
    }
    "#,
            );
    }

    #[test]
    fn convert_vec_2() {
        check_assist(
            convert_unsafe_to_safe,
            r#"
    fn main() {

        let cap = 100;

        let mut buffer = Vec::with_capacity(cap);

        unsafe$0 {
            buffer.set_len(cap); 
        }
        input.read_into(&mut buffer);
    }
    "#,
                r#"
    fn main() {

        let cap = 100;

        let mut buffer = vec![0; cap];

        input.read_into(&mut buffer);
    }
    "#,
            );
    }

    #[test]
    fn convert_vec_3() {
        check_assist(
            convert_unsafe_to_safe,
            r#"
    fn main() {

        let cap = 100;

        let mut buffer = Vec::with_capacity(cap);
        unsafe$0 {
            buffer.set_len(cap); 
        }
    }
    "#,
                r#"
    fn main() {

        let cap = 100;

        let mut buffer = Vec::with_capacity(cap);
        unsafe$0 {

            buffer.set_len(cap); 
        }
    }
    "#,
            );
    }
}
