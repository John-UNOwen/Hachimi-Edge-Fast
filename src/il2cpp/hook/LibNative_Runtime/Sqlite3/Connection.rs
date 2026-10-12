use std::sync::Mutex;

use fnv::FnvHashMap;
use once_cell::sync::Lazy;
use sqlparser::{
    dialect::SQLiteDialect,
    keywords::Keyword,
    parser::Parser,
    tokenizer::{Token, Tokenizer}
};

use crate::core::hachimi::recover_lock;
use crate::il2cpp::{
    api::{il2cpp_object_new, il2cpp_runtime_object_init},
    ext::Il2CppStringExt,
    sql::{self, SelectExt, SelectItemExt},
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

// The `column = ?` predicates after the statement's first WHERE, each with the number SQLite gives its placeholder.
// SQLite numbers the placeholders of the whole statement in the order they are written: a `?` takes one more than the
// largest number so far, and `?NNN` takes NNN. The old counter advanced only for predicates of that one shape, so an
// `IN (?, ?)` shifted every later number (C21).
fn where_placeholders(sql: &str) -> Vec<(String, i32)> {
    let dialect = SQLiteDialect {};
    let Ok(tokens) = Tokenizer::new(&dialect, sql).tokenize() else {
        return Vec::new();
    };

    // A named parameter (`:c`, `@c` or `$c`) takes its number in SQLite's order of first use, which this reader does
    // not follow. sqlparser reads `:` and `@` as tokens of their own and `$` as the start of a word, so all three are
    // caught here. A statement that names one records no column, and the query passes through with the game's own text (C21).
    let named_elsewhere = tokens.iter().any(|token| {
        matches!(token, Token::Colon | Token::AtSign)
            || matches!(token, Token::Word(word) if word.quote_style.is_none() && word.value.starts_with('$'))
    });
    if named_elsewhere {
        return Vec::new();
    }

    let tokens: Vec<Token> = tokens.into_iter().filter(|token| !matches!(token, Token::Whitespace(_))).collect();

    let where_at = tokens.iter().position(|token| {
        matches!(token, Token::Word(word) if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("WHERE"))
    });

    let mut placeholders = Vec::new();
    let mut largest = 0;

    for (at, token) in tokens.iter().enumerate() {
        let Token::Placeholder(text) = token else {
            continue;
        };

        let idx = match text.strip_prefix('?') {
            Some("") => {
                largest += 1;
                largest
            }
            Some(digits) => {
                let Ok(idx) = digits.parse::<i32>() else {
                    continue;
                };
                largest = largest.max(idx);
                idx
            }
            // only `?` placeholders get this far: a named parameter returned above
            None => continue,
        };

        // Only a placeholder after the first WHERE, behind an unqualified `column =`, names a column
        if !where_at.is_some_and(|start| at > start) || at < 2 || !matches!(tokens[at - 1], Token::Eq) {
            continue;
        }
        let qualified = at >= 3 && matches!(tokens[at - 3], Token::Period);
        if let (false, Token::Word(word)) = (qualified, &tokens[at - 2]) {
            placeholders.push((word.value.clone(), idx));
        }
    }

    placeholders
}

#[inline(never)]
fn parse_query(query: *mut Il2CppObject, sql: *const Il2CppString) {
    // C8: `query` is the object the game just handed back. IL2CPP reuses the address of a freed query, and a query freed
    // without `Dispose` leaves its state under that address, so this one starts without the state of whatever held the
    // address before it. `Dispose` removes the state of the queries it sees go.
    recover_lock(&SELECT_QUERIES).remove(&(query as usize));

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

        // Add params. The game binds them by the numbers SQLite gives its placeholders, which run over the whole statement
        // in the order they are written, so a predicate of any other shape still takes its numbers (C21).
        for (name, idx) in where_placeholders(&sql_str) {
            query_state.add_param(idx, &name);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_predicates_after_where_take_the_numbers_sqlite_gives_their_placeholders() {
        // The `IN (?, ?)` takes two numbers, so `index` is the fourth placeholder and not the second.
        assert_eq!(
            where_placeholders("SELECT text FROM text_data WHERE category = ? AND id IN (?, ?) AND index = ?"),
            vec![("category".to_owned(), 1), ("index".to_owned(), 4)]
        );
    }

    #[test]
    fn a_placeholder_inside_a_string_is_not_one() {
        assert_eq!(
            where_placeholders("SELECT text FROM text_data WHERE note = '? = ?' AND category = ?"),
            vec![("category".to_owned(), 1)]
        );
    }

    #[test]
    fn numbered_placeholders_take_the_numbers_sqlite_gives_them() {
        // `?3` names the third, and each bare `?` after it takes one more than the largest number so far.
        assert_eq!(
            where_placeholders("SELECT text FROM text_data WHERE category = ?3 AND index = ? AND id = ?"),
            vec![("category".to_owned(), 3), ("index".to_owned(), 4), ("id".to_owned(), 5)]
        );
    }

    #[test]
    fn a_named_parameter_records_no_column_rather_than_a_guessed_one() {
        for sql in [
            "SELECT text FROM text_data WHERE category = :c AND index = ?",
            "SELECT text FROM text_data WHERE category = @c AND index = ?",
            "SELECT text FROM text_data WHERE category = $c AND index = ?",
        ] {
            assert!(where_placeholders(sql).is_empty(), "{sql}");
        }
    }

    #[test]
    fn a_qualified_column_is_not_a_predicate_this_reader_names() {
        assert!(where_placeholders("SELECT text FROM text_data WHERE text_data.category = ?").is_empty());
    }

    #[test]
    fn a_new_query_at_a_reused_address_starts_without_the_old_state() {
        let address = 0x0C8_0000_0010usize;
        recover_lock(&SELECT_QUERIES).insert(address, Box::new(sql::TextDataQuery::default()));

        parse_query(address as *mut Il2CppObject, std::ptr::null());

        assert!(!recover_lock(&SELECT_QUERIES).contains_key(&address), "the freed query's state survived the reuse");
    }
}
