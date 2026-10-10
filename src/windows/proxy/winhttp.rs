#![allow(non_snake_case, non_upper_case_globals)]

use widestring::{U16CString, Utf16Str};
use windows::{core::PCWSTR, Win32::System::LibraryLoader::LoadLibraryW};

use crate::windows::utils;

proxy_table! {
    module = "winhttp.dll" ;
    WinHttpAddRequestHeaders, WinHttpAddRequestHeaders_orig ;
    WinHttpCheckPlatform, WinHttpCheckPlatform_orig ;
    WinHttpCloseHandle, WinHttpCloseHandle_orig ;
    WinHttpConnect, WinHttpConnect_orig ;
    WinHttpCrackUrl, WinHttpCrackUrl_orig ;
    WinHttpCreateProxyResolver, WinHttpCreateProxyResolver_orig ;
    WinHttpCreateUrl, WinHttpCreateUrl_orig ;
    WinHttpDetectAutoProxyConfigUrl, WinHttpDetectAutoProxyConfigUrl_orig ;
    WinHttpFreeProxyResult, WinHttpFreeProxyResult_orig ;
    WinHttpFreeProxyResultEx, WinHttpFreeProxyResultEx_orig ;
    WinHttpFreeProxySettings, WinHttpFreeProxySettings_orig ;
    WinHttpGetDefaultProxyConfiguration, WinHttpGetDefaultProxyConfiguration_orig ;
    WinHttpGetIEProxyConfigForCurrentUser, WinHttpGetIEProxyConfigForCurrentUser_orig ;
    WinHttpGetProxyForUrl, WinHttpGetProxyForUrl_orig ;
    WinHttpGetProxyForUrlEx, WinHttpGetProxyForUrlEx_orig ;
    WinHttpGetProxyForUrlEx2, WinHttpGetProxyForUrlEx2_orig ;
    WinHttpGetProxyResult, WinHttpGetProxyResult_orig ;
    WinHttpGetProxyResultEx, WinHttpGetProxyResultEx_orig ;
    WinHttpGetProxySettingsVersion, WinHttpGetProxySettingsVersion_orig ;
    WinHttpOpen, WinHttpOpen_orig ;
    WinHttpOpenRequest, WinHttpOpenRequest_orig ;
    WinHttpQueryAuthSchemes, WinHttpQueryAuthSchemes_orig ;
    WinHttpQueryDataAvailable, WinHttpQueryDataAvailable_orig ;
    WinHttpQueryHeaders, WinHttpQueryHeaders_orig ;
    WinHttpQueryOption, WinHttpQueryOption_orig ;
    WinHttpReadData, WinHttpReadData_orig ;
    WinHttpReadProxySettings, WinHttpReadProxySettings_orig ;
    WinHttpReceiveResponse, WinHttpReceiveResponse_orig ;
    WinHttpResetAutoProxy, WinHttpResetAutoProxy_orig ;
    WinHttpSendRequest, WinHttpSendRequest_orig ;
    WinHttpSetCredentials, WinHttpSetCredentials_orig ;
    WinHttpSetDefaultProxyConfiguration, WinHttpSetDefaultProxyConfiguration_orig ;
    WinHttpSetOption, WinHttpSetOption_orig ;
    WinHttpSetStatusCallback, WinHttpSetStatusCallback_orig ;
    WinHttpSetTimeouts, WinHttpSetTimeouts_orig ;
    WinHttpTimeFromSystemTime, WinHttpTimeFromSystemTime_orig ;
    WinHttpTimeToSystemTime, WinHttpTimeToSystemTime_orig ;
    WinHttpWebSocketClose, WinHttpWebSocketClose_orig ;
    WinHttpWebSocketCompleteUpgrade, WinHttpWebSocketCompleteUpgrade_orig ;
    WinHttpWebSocketQueryCloseStatus, WinHttpWebSocketQueryCloseStatus_orig ;
    WinHttpWebSocketReceive, WinHttpWebSocketReceive_orig ;
    WinHttpWebSocketSend, WinHttpWebSocketSend_orig ;
    WinHttpWebSocketShutdown, WinHttpWebSocketShutdown_orig ;
    WinHttpWriteData, WinHttpWriteData_orig ;
    WinHttpWriteProxySettings, WinHttpWriteProxySettings_orig ;
}

pub fn init(system_dir: &Utf16Str) {
    unsafe {
        let dll_path = system_dir.to_owned() + "\\winhttp.dll";
        let dll_path_cstr = U16CString::from_vec(dll_path.into_vec()).unwrap();
        let handle = LoadLibraryW(PCWSTR(dll_path_cstr.as_ptr())).expect("winhttp.dll");

        WinHttpAddRequestHeaders_orig = utils::get_proc_address(handle, c"WinHttpAddRequestHeaders");
        WinHttpCheckPlatform_orig = utils::get_proc_address(handle, c"WinHttpCheckPlatform");
        WinHttpCloseHandle_orig = utils::get_proc_address(handle, c"WinHttpCloseHandle");
        WinHttpConnect_orig = utils::get_proc_address(handle, c"WinHttpConnect");
        WinHttpCrackUrl_orig = utils::get_proc_address(handle, c"WinHttpCrackUrl");
        WinHttpCreateProxyResolver_orig = utils::get_proc_address(handle, c"WinHttpCreateProxyResolver");
        WinHttpCreateUrl_orig = utils::get_proc_address(handle, c"WinHttpCreateUrl");
        WinHttpDetectAutoProxyConfigUrl_orig = utils::get_proc_address(handle, c"WinHttpDetectAutoProxyConfigUrl");
        WinHttpFreeProxyResult_orig = utils::get_proc_address(handle, c"WinHttpFreeProxyResult");
        WinHttpFreeProxyResultEx_orig = utils::get_proc_address(handle, c"WinHttpFreeProxyResultEx");
        WinHttpFreeProxySettings_orig = utils::get_proc_address(handle, c"WinHttpFreeProxySettings");
        WinHttpGetDefaultProxyConfiguration_orig = utils::get_proc_address(handle, c"WinHttpGetDefaultProxyConfiguration");
        WinHttpGetIEProxyConfigForCurrentUser_orig = utils::get_proc_address(handle, c"WinHttpGetIEProxyConfigForCurrentUser");
        WinHttpGetProxyForUrl_orig = utils::get_proc_address(handle, c"WinHttpGetProxyForUrl");
        WinHttpGetProxyForUrlEx_orig = utils::get_proc_address(handle, c"WinHttpGetProxyForUrlEx");
        WinHttpGetProxyForUrlEx2_orig = utils::get_proc_address(handle, c"WinHttpGetProxyForUrlEx2");
        WinHttpGetProxyResult_orig = utils::get_proc_address(handle, c"WinHttpGetProxyResult");
        WinHttpGetProxyResultEx_orig = utils::get_proc_address(handle, c"WinHttpGetProxyResultEx");
        WinHttpGetProxySettingsVersion_orig = utils::get_proc_address(handle, c"WinHttpGetProxySettingsVersion");
        WinHttpOpen_orig = utils::get_proc_address(handle, c"WinHttpOpen");
        WinHttpOpenRequest_orig = utils::get_proc_address(handle, c"WinHttpOpenRequest");
        WinHttpQueryAuthSchemes_orig = utils::get_proc_address(handle, c"WinHttpQueryAuthSchemes");
        WinHttpQueryDataAvailable_orig = utils::get_proc_address(handle, c"WinHttpQueryDataAvailable");
        WinHttpQueryHeaders_orig = utils::get_proc_address(handle, c"WinHttpQueryHeaders");
        WinHttpQueryOption_orig = utils::get_proc_address(handle, c"WinHttpQueryOption");
        WinHttpReadData_orig = utils::get_proc_address(handle, c"WinHttpReadData");
        WinHttpReadProxySettings_orig = utils::get_proc_address(handle, c"WinHttpReadProxySettings");
        WinHttpReceiveResponse_orig = utils::get_proc_address(handle, c"WinHttpReceiveResponse");
        WinHttpResetAutoProxy_orig = utils::get_proc_address(handle, c"WinHttpResetAutoProxy");
        WinHttpSendRequest_orig = utils::get_proc_address(handle, c"WinHttpSendRequest");
        WinHttpSetCredentials_orig = utils::get_proc_address(handle, c"WinHttpSetCredentials");
        WinHttpSetDefaultProxyConfiguration_orig = utils::get_proc_address(handle, c"WinHttpSetDefaultProxyConfiguration");
        WinHttpSetOption_orig = utils::get_proc_address(handle, c"WinHttpSetOption");
        WinHttpSetStatusCallback_orig = utils::get_proc_address(handle, c"WinHttpSetStatusCallback");
        WinHttpSetTimeouts_orig = utils::get_proc_address(handle, c"WinHttpSetTimeouts");
        WinHttpTimeFromSystemTime_orig = utils::get_proc_address(handle, c"WinHttpTimeFromSystemTime");
        WinHttpTimeToSystemTime_orig = utils::get_proc_address(handle, c"WinHttpTimeToSystemTime");
        WinHttpWebSocketClose_orig = utils::get_proc_address(handle, c"WinHttpWebSocketClose");
        WinHttpWebSocketCompleteUpgrade_orig = utils::get_proc_address(handle, c"WinHttpWebSocketCompleteUpgrade");
        WinHttpWebSocketQueryCloseStatus_orig = utils::get_proc_address(handle, c"WinHttpWebSocketQueryCloseStatus");
        WinHttpWebSocketReceive_orig = utils::get_proc_address(handle, c"WinHttpWebSocketReceive");
        WinHttpWebSocketSend_orig = utils::get_proc_address(handle, c"WinHttpWebSocketSend");
        WinHttpWebSocketShutdown_orig = utils::get_proc_address(handle, c"WinHttpWebSocketShutdown");
        WinHttpWriteData_orig = utils::get_proc_address(handle, c"WinHttpWriteData");
        WinHttpWriteProxySettings_orig = utils::get_proc_address(handle, c"WinHttpWriteProxySettings");
    }
}