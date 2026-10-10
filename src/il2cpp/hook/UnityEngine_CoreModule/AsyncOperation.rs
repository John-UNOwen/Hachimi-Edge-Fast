use std::ptr::null_mut;

use crate::il2cpp::{api::il2cpp_class_from_il2cpp_type, symbols::{get_field_from_name, get_method_addr}, types::*};

static mut ADD_COMPLETED_ADDR: usize = 0;
impl_addr_wrapper_fn!(add_completed, ADD_COMPLETED_ADDR, (), this: *mut Il2CppObject, value: *mut Il2CppDelegate);

pub static mut ACTION_ASYNCOPERATION_CLASS: *mut Il2CppClass = null_mut();

pub fn init(UnityEngine_CoreModule: *const Il2CppImage) {
    get_class_or_return!(UnityEngine_CoreModule, UnityEngine, AsyncOperation);

    let m_completeCallback_field = get_field_from_name(AsyncOperation, c"m_completeCallback");

    // C9: `get_field_from_name` says a name this client has no field for by handing back null, and a
    // null handle has no `type_` to read - that would be a load from address 8 inside `init`. The
    // delegate class stays null, which is what every user of it already treats as "no class here".
    if m_completeCallback_field.is_null() {
        return;
    }

    unsafe {
        ADD_COMPLETED_ADDR = get_method_addr(AsyncOperation, c"add_completed", 1);
        ACTION_ASYNCOPERATION_CLASS = il2cpp_class_from_il2cpp_type((*m_completeCallback_field).type_);
    }
}