use std::sync::Mutex;

use fnv::FnvHashMap;
use once_cell::sync::Lazy;
use sqlparser::{
    ast::BinaryOperator,
    dialect::SQLiteDialect,
    keywords::Keyword,
    parser::Parser
};

use crate::core::hachimi::recover_lock;
use crate::il2cpp::{
    api::{il2cpp_object_new, il2cpp_runtime_object_init},
    ext::Il2CppStringExt,
    sql::{self, ExprExt, SelectExt, SelectItemExt},
    symbols::get_method_addr,
    types::*
};

static mut CLASS: *mut Il2CppClass = std::ptr::null_mut();
pub fn class() -> *mut Il2CppClass {
    unsafe { CLASS }
}

pub fn new() -> *mut Il2CppObject {
    // C9: the class is this hook's own (null when the client has no `LibNative.Sqlite3.Connection`)
    // and the object is the game's allocation, which the game may refuse. Neither has anything to
    // initialise, and every caller of `new` already copes with a null connection.
    if class().is_null() {
        return std::ptr::null_mut();
    }

    let object = il2cpp_object_new(class());
    if object.is_null() {
        return std::ptr::null_mut();
    }

    il2cpp_runtime_object_init(object);
    object
}

// C2: `recover_lock` is the shape AGENTS section 6 asks for in a detour: a lock poisoned by an
// earlier panic keeps handing out its data instead of panicking across the FFI boundary on
// every later game query.
pub static SELECT_QUERIES: Lazy<Mutex<FnvHashMap<usize, Box<dyn sql::SelectQueryState + Send + Sync>>>> =
    Lazy::new(|| Mutex::new(FnvHashMap::default()));

#[inline(never)]
fn parse_query(query: *mut Il2CppObject, sql: *const Il2CppString) {
    // C9: `sql` is the string the game handed its own `Query`/`PreparedQuery`, and a call that
    // carries none is a statement this hook has nothing to read a table name out of.
    if sql.is_null() {
        return;
    }

    let sql_str = unsafe { (*sql).as_utf16str() }.to_string();

    // quick escape!!!11
    if !sql_str.starts_with("SELECT") {
        return;
    }

    // parse the sql string
    let dialect = SQLiteDialect {};
    let parser_res = Parser::new(&dialect).try_with_sql(&sql_str);

    if let Ok(mut parser) = parser_res {
        // only care about select statements
        if !parser.parse_keyword(Keyword::SELECT) {
            return;
        }
        let Ok(select) = parser.parse_select() else {
            return;
        };

        // and their first table name (SELECT FROM table_name)
        let Some(table_name) = select.get_first_table_name() else {
            debug!("no table name");
            return;
        };

        // Create the query state
        let mut query_state: Box<dyn sql::SelectQueryState + Send + Sync> = match table_name.as_ref() {
            "text_data" => Box::new(sql::TextDataQuery::default()),
            "character_system_text" => Box::new(sql::CharacterSystemTextQuery::default()),
            "race_jikkyo_comment" => Box::new(sql::RaceJikkyoCommentQuery::default()),
            "race_jikkyo_message" => Box::new(sql::RaceJikkyoMessageQuery::default()),
            _ => return
        };

        // Add columns
        let mut i = 0;
        for item in select.projection.iter() {
            if let Some(name) = item.get_unnamed_expr_ident() {
                query_state.add_column(i, name);
                i += 1;
            }
        }

        // Add params
        i = 1; // index starts at 1
        if let Some(selection) = select.selection {
            // this should visit them in order (column1 = ? AND column2 = ? ...)
            for expr in selection.binary_op_iter() {
                if *expr.op != BinaryOperator::Eq { continue; }

                if let Some(name) = expr.left.get_ident_value() {
                    if expr.right.is_placeholder_value() {
                        query_state.add_param(i, name);
                        i += 1;
                    }
                }
            }
        }

        // Add query state
        recover_lock(&SELECT_QUERIES).insert(query as usize, query_state);
    }
}

type QueryFn = extern "C" fn(this: *mut Il2CppObject, sql: *const Il2CppString) -> *mut Il2CppObject;
def_detour! {
    pub Query(this: *mut Il2CppObject, sql: *const Il2CppString) -> *mut Il2CppObject {
            trace!("Query");
        // Every reader in `il2cpp::sql.rs` opens its query through this wrapper rather than
        // through the game's own call, and each of them already tests the pointer it gets back.
        // A null query is therefore the inert answer when `init` resolved no `Query`: no rows,
        // `Dispose` is not reached, `CloseDB` still runs (C1).
        let Some(query_orig) = get_orig_fn_guarded!(Query, QueryFn) else {
            return std::ptr::null_mut();
        };

        let query = query_orig(this, sql);
        parse_query(query, sql);
        query
    }
}

type PreparedQueryFn = extern "C" fn(this: *mut Il2CppObject, sql: *const Il2CppString) -> *mut Il2CppObject;
def_detour! {
    PreparedQuery(this: *mut Il2CppObject, sql: *const Il2CppString) -> *mut Il2CppObject {
            trace!("PreparedQuery");
        let query = get_orig_fn!(PreparedQuery, PreparedQueryFn)(this, sql);
        parse_query(query, sql);
        query
    }
}

static mut OPEN_ADDR: usize = 0;
impl_addr_wrapper_fn!(Open, OPEN_ADDR, bool,
    this: *mut Il2CppObject, fileName: *mut Il2CppString, vfsName: *mut Il2CppString, key: *mut Il2CppArray, cipherType: i32
);

static mut CLOSEDB_ADDR: usize = 0;
impl_addr_wrapper_fn!(CloseDB, CLOSEDB_ADDR, (), this: *mut Il2CppObject);

pub fn init(LibNative_Runtime: *const Il2CppImage) {
    get_class_or_return!(LibNative_Runtime, "LibNative.Sqlite3", Connection);

    let Query_addr = get_method_addr(Connection, c"Query", 1);
    let PreparedQuery_addr = get_method_addr(Connection, c"PreparedQuery", 1);

    new_hook!(Query_addr, Query);
    new_hook!(PreparedQuery_addr, PreparedQuery);

    unsafe {
        CLASS = Connection;
        OPEN_ADDR = get_method_addr(Connection, c"Open", 4);
        CLOSEDB_ADDR = get_method_addr(Connection, c"CloseDB", 0);
    }
}