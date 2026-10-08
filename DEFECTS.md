# Defect ledger

Ledger for known defects in this fork. Not a changelog. Status marks:

- `[x]` fixed and verified in source
- `[~]` partially fixed, remainder listed
- `[ ]` open
- `[latent]` code path is defective but inert on the current client

Baseline build for this ledger: `e984b93`, deployed as `cri_mana_vpx.dll`, 40,142,336 bytes,
SHA256 `5C58010A45703B0DE38AD43797BEA090B4B9552FDFA66F3F04527FC2410D9D8C`.
Client: Umamusume Pretty Derby, Global region, Steam, Unity 2022.3.62f2 IL2CPP.

## A. Runtime findings, 2026-10-08 run

Evidence: `hachimi.log` 114.3 KB / 1136 lines, session 03:10:10 to 03:16:04 local,
and `hachimi/introspect.log` 1896.1 KB dumped by the same build.

- [ ] **A1 `NowLoading::PlayFadeNowLoading` scales the wrong arguments.** 31 calls were logged
  as `NowLoading::PlayFadeNowLoading(0, 1, 0.3)` x15 and `(1, 0, 0.3)` x16. Arguments 0 and 1
  are the fade endpoints (alpha to and alpha from), argument 2 is the duration. The hook in
  `src/il2cpp/hook/umamusume/NowLoading.rs` scales every float it receives, so at factor 20 the
  target alpha becomes 0.05 instead of 1. Fix: scale index 2 only.
- [ ] **A2 Overload matching is too coarse.** `symbols::get_method_overload` compares
  `Il2CppTypeEnum` only, and `CLASS` matches every reference type. The dump shows
  `SingleModeResultContentBase::FadeInContent/4` is not a duplicate pair but distinct overloads
  taking `UnityEngine.UI.MaskableGraphic` versus `UnityEngine.CanvasGroup`, and
  `FadeInContentFromBottom/4` takes `CanvasGroup` versus `Gallop.TextCommon`. Two of three are
  unhooked. No `FadeInContent*` duration line appears in a session that contained several
  training result screens. Fix: match parameter class names, not type enums.
- [ ] **A3 Static helpers are currently not hooked.** The new static guard rejected two targets:
  `StoryTimelineController::GetTimeScaleByHighSpeedType/1 -> static float(bool)` and
  `GetTimeScaleHighSpeed/1 -> static float(bool)`. The guard prevented a wrapper that reserves a
  register for `this` from reading the `bool` from the wrong register. `StoryViewController::GetTimeScaleByHighSpeedType/0 -> static float()` confirms the existing
  no-`this` wrapper in `StoryViewController.rs` is correct. Fix: add argument-only wrappers for
  static targets.
- [ ] **A4 Only 2 of 13 installed scaling points were reached.** Fired:
  `CountupModifier_getDuration 0.16 -> 0.008` and
  `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5`. Installed with no call in this session:
  `TextModifier_getDuration`, `TextModifier_getDelay`, `TrainingFooter_GetCloseAnimWaitTime`,
  `TrainingFooter_GetItemAnimDuration`, `TrainingCuttClip_getDelayTime`,
  `SingleModeUtils_GetHighSpeedPlayDuration`, `SingleModeUtils_GetCutTimeScale`,
  `StoryTimeline_getTimeScaleEventWipe`, `TeamStadiumGrandResult_FadeInContentFromRight`,
  `FadeInContent`, `FadeInContentFromRight`, `FadeInContentFromBottom`,
  `SetHighSpeedFrameCount/1 (int)`, `ActivateSkipButton`, `PlayInNowLoading`, `PlayOutNowLoading`.
- [ ] **A5 `PlayFadeFrontCanvas` is now hookable.** The dump resolves the blocked value type:
  `Gallop.NowLoading::PlayFadeFrontCanvas/5 -> void(float, float, float, class<System.Action>, struct<DG.Tweening.Ease:4B>)`.
  A 4 byte enum travels in a general purpose register, not XMM, so it can be passed through.
  Same applies to `StoryViewController::GetTimeScaleByHighSpeedType/1 -> static float(struct<Gallop.StoryTimelineController.HighSpeedType:4B>)`.
- [ ] **A6 `Show` / `Hide` still do not resolve.** `show_addr is null` and `hide_addr is null`
  persist, so `hide_now_loading` is inert. `NowLoading.rs:103-104` searches arity 8 and 4, which
  is the Japanese shape. An earlier dump reported `Show/7` and `Hide/3` on this client; this run's
  dump lists only `PlayFadeNowLoading`, `PlayFadeFrontCanvas`, `SetupCrossFadeAsync` and
  `PlayCrossFadeAsync` for the class, so the arity needs a targeted dump before the wrappers are
  rewritten. See A8.
- [ ] **A7 `byref` parameters are still ignored** by the overload matcher, for example
  `GetNextFrameCount_HighSpeed/2 -> void(float&, int&)`.
- [ ] **A8 The introspection dump is silently truncated.** `MAX_FULL_CLASSES = 500`
  (`src/il2cpp/introspect.rs:60`) was hit exactly, with no truncation marker, while the same run
  reported 10 name resolution failures. Absence of a name in `introspect.log` does not prove the
  name is absent from the client.
- [ ] **A9 The between-scene wipe is coroutine gated, and the load is the remaining cost.**
  `Gallop.NowLoading::SetupCrossFadeAsync/1 -> class<System.Collections.IEnumerator>(class<System.Action>)`,
  `PlayCrossFadeAsync/1 -> class<System.Collections.IEnumerator>(class<System.Action>)` and field
  `_isCrossFade [bool]`. The tween is DOTween based and the wait is a coroutine, so `ui_animation_scale`
  and `time_scale` already act on both. Measured residual per transition is 0.33 s to 3.2 s of
  async load, which no duration hook can shorten.
- [x] **A9 The static guard and the first hit logger both worked as designed** (2 rejections,
  2 hits logged).

### Measured baseline, transition cost

15 complete fade pairs (fade to black then fade from black) across 260.2 s of play:
gaps in ms 1139, 978, 650, 730, 2901, 326, 794, 2647, 337, 2164, 605, 1079, 3177, 392, 1024.
min 326 ms, median 1024 ms, max 3177 ms, total 18.9 s. With both fades at 0.3 s shipped, at most
about 9 s of that window is animation, and both fades are DOTween based (`DG.Tweening.Ease` in the
signature) so they were already compressed by `ui_animation_scale` before this feature existed.
Any future claim of speedup must be measured against these numbers, not against feel.

## B. Open items from the animation feature review

- [x] duplicate detour on `StoryViewController::GetTimeScaleByHighSpeedType` removed
- [x] static targets rejected in `AnimationSpeed::resolve_method`
- [x] `scale_frame_count` put to use by `SetHighSpeedFrameCount`
- [ ] A2 overload matching
- [ ] A6 `Show` / `Hide` arity
- [ ] A7 `byref`
- [ ] struct typed parameters still skipped in general; A5 shows which ones are actually safe

## C. Safety review findings (31)

- [~] **C1 Calls through address 0.** Closed for `def_method_wrapper_fn!`,
  `impl_addr_wrapper_fn!` and both field accessor macro families. Still open: `get_orig_fn!`
  transmutes the 0 trampoline the interceptor returns for an uninstalled hook (234 sites, guard
  exists only in `UnityEngine_CoreModule/Time.rs:49-52`), and the 45 exported `WinHttp*` stubs plus
  `UnityMain` are `jmp qword ptr [rip + <orig>]` through `static mut _orig: usize = 0` while
  `src/windows/hook.rs:50-62` skips proxy init for this module name.
- [ ] **C2 No panic or SEH barrier at any hook boundary.** `catch_unwind` appears only in
  `src/core/gui.rs`; hook bodies call `unwrap()` on shared mutexes (poisoning makes every later
  call of that hook panic), `Hachimi::instance()` calls `process::exit(1)` when uninitialized
  (`src/core/hachimi.rs:139-143`), and `src/windows/hachimi_impl.rs:30` kills
  `UnityCrashHandler64.exe`, so an abort leaves no dump.
- [ ] **C3 Live Theater duplicate character.** `LiveTheaterCharaSelect.rs:11-13` skips
  `CheckSwapChara` and `LiveTheaterViewController.rs:7-17` returns
  `ChangeLive_onSuccess(this, null)`, a client side success branch with no server response.
  Depends on compiler generated symbol names that move with game updates.
- [ ] **C4 Injection point.** The mod sits at the game root as `cri_mana_vpx.dll`, shadowing the
  shipped CRI codec at `UmamusumePrettyDerby_Data\Plugins\x86_64\cri_mana_vpx.dll`, which never
  loads. No export forwarding, and Steam integrity verification does not remove a root file the
  game never shipped.
- [ ] **C5 Unclamped speed layers.** `ui_animation_scale` allows 0.1..=1000.0 in the GUI
  (`src/core/gui.rs:5506`) with a code default of 1.0, and `DOTween/TweenManager.rs:3-10`
  multiplies both `delta_time` and `independent_time` with no clamp. `independent_time` is the
  channel DOTween leaves unscaled so UI keeps animating while the game is paused.
- [ ] **C6 Purchase path.** `PaymentUtility.rs:11-27` shows a Yes/No dialog that only queues,
  then calls the original `StartPurchase` unconditionally, and the Yes branch posts
  `PostMessageW(None, WM_CLOSE, 0, 0)` and sets `disable_gui_once`. Installed whenever the Steam
  overlay conflicts, which is the ordinary case.
- [ ] **C7 Resolution by name plus argument count** outside `AnimationSpeed`, with hand written
  wrapper signatures at each site.
- [ ] **C8 Mod state keyed on raw object addresses** while IL2CPP recycles freed addresses
  (`Sqlite3/Connection.rs:97,102-115` and others).
- [ ] **C9 Unchecked dereferences of game returned values** (`AssetBundle.rs:79-80` and others).
- [ ] **C10 Asset patch identity guard disabled** for the Global target while
  `AssetBundle.LoadAsset_Internal` is hooked for every asset load.
- [ ] **C11 Check then use race** in the live translation apply path
  (`Text.rs:96-117`, `TextMesh.rs:38`).
- [ ] **C12 `Time::set_timeScale` overwrite** is a simulation lever rather than an animation lever.
- [latent] **C13 Static duration constant re-assertion** in `AnimationSpeed` writes
  `static readonly` constants every `apply()` and adopts values the game wrote. Inert on this
  client: 0 of 61 fields resolve, all 61 rejected as literal or non static.
- [ ] **C14 Options that do nothing on this client** while still offered in the GUI:
  `hide_now_loading` (A6) and `ui_loading_show_orientation_guide`.
- [ ] **C15 Coroutine aborts are class wide** because `symbols.rs:266-299` patches `MoveNext` on
  the compiler generated enumerator class.
- [ ] **C16 `on_game_initialized()` runs twice** when `ui_scale != 1.0`.
- [ ] **C17 The input layer is suppressed** by 11 hooks during free camera capture, gated on a
  global mutable scene flag with two reset points (`InputSystemManager.rs:26-91`).
- [ ] **C18 Unauthenticated local control plane.** `src/core/ipc.rs:13-19` binds 0.0.0.0:50433
  when `ipc_listen_all` is true, and the plane can call `SoftReset`, `StoryGotoBlock` and
  `ReloadLocalizedData`. Currently `enable_ipc: false`.
- [ ] **C19 Third party sourced URLs and text reach game surfaces.** `WebViewManager::GetUrl`
  substitutes `news_url` from the downloaded translation repo config, and
  `UnityEngine_CoreModule/Application.rs:49-55` suppresses `Application::OpenURL` when the mod
  WebView opens. Full URLs including query parameters are written to the log
  (`src/windows/webview.rs:270`) and every gacha URL is stored in an unbounded map.
- [ ] **C20 Native SQLite interception of the game's encrypted databases.**
  `src/core/hachimi.rs:19-46,345-357` hooks `sqlite3_open_v2` and `sqlite3_key`, keeps the raw
  key in `RETRIEVED_RAW_KEY` (`src/il2cpp/sql.rs:12-13`), and applies it to whichever handle next
  consumes a global one-shot flag.
- [ ] **C21 SQL bind ordinals can be recorded against the wrong column** because the parameter
  counter only advances for `identifier = ?` predicates (`Sqlite3/Connection.rs:63-69,102-115`,
  `src/il2cpp/sql.rs:137-154,793-822`). Substitution inside the game's data path, not a UI layer.
- [ ] **C22 `story_tcps_multiplier` compounds** on repeated loads of the same asset instance
  (`StoryTimelineData.rs:104-109` reached from both `AssetBundle.rs:59` and
  `AssetBundleRequest.rs:17`) with no applied marker, GUI range 0.1..=1000.0, live value 1000.0.
- [ ] **C23 Story block length recomputation can shorten a block** (
  `StoryTimelineData.rs:340-341` writes `StartFrame + newClipLength + 1` with no
  `max(originalBlockLength, ...)`), cutting trailing frames of longer non text tracks.
- [ ] **C24 Story choice auto select applies the same factor twice.**
  `StoryChoiceController.rs:23-44` and `StoryViewController.rs:8-14` both multiply by
  `0.75 / story_choice_auto_select_delay` (live delay 0.0001, so 7500x), the constant 0.75 is
  assumed rather than read from the client, `IS_CHECKING_CHOICE_AUTO_TAP.swap(false)` consumes the
  flag on the first nested getter call, and only `/0` of the two live overloads is hooked.
- [ ] **C25 About 5.5 s of work runs inside `DllMain` under the loader lock**: 198 hook install
  requests, a 1.9 MB dump written into Program Files, native sqlite hooking, window subclassing
  plus a CBT hook, a Discord IPC pipe connect that fails, and `TerminateProcess` of another
  process by image name with no ownership check.
- [ ] **C26 Outbound update check plus unsigned installer execution path**, live by default,
  from inside the game process (`src/core/hachimi.rs:560-571`, `src/core/updater.rs:151-192`).
- [ ] **C27 `disabled_hooks` matches bare wrapper names** that are not unique across modules
  (`Hide` appears in 4 files, `Update` in 3), `new_hook!` logs an install line before the null
  check, and `Interceptor::hook` keys on the hook function address so a reused wrapper would call
  the first target's trampoline.
- [ ] **C28 Race replay seeking in Global leaves used-skill state unrepaired** because the resync
  stage returns early unless region is Japan (`RaceManagerReplayBase.rs:71-72,110-112`).
- [ ] **C29 All mod state lives inside the Steam managed install tree** (`hachimi\config.json`,
  `hachimi.db`, `introspect.log`, generated story dictionaries, `<bundle>.a.png` beside shipped
  bundles), and extreme speed values are re-applied on the next load of any copy.
- [ ] **C30 No structured resolution report** and a failed file logger is silent
  (`src/windows/log_impl.rs:9-24`).
- [latent] **C31 Android only hooks that invert the game's legality checks**
  (`Cute_Core_Assembly/Device.rs` `IsIllegalUser`, `SafetyNet.rs:16` passing the success callback
  as the failure callback). Not compiled into the Windows deliverable
  (`src/il2cpp/hook/mod.rs:198-199`), but they ship in this tree.

## D. Fix order

1. A1 duration argument index in `NowLoading`.
2. A2 overload matching by parameter class name.
3. A3 argument-only wrappers for static targets, then re-enable both high speed helpers.
4. A5 hook `PlayFadeFrontCanvas` and the `HighSpeedType` enum overload.
5. C2 unwind and SEH barrier at hook entry, `unwrap_or_else` on shared mutexes, bypass instead of
   `process::exit`.
6. C5 clamp `ui_animation_scale` in code and leave `independent_time` alone.
7. C22 and C24, the two compounding multipliers that are live in the current config.
8. C1 remaining `get_orig_fn!` audit and the proxy export stubs.
9. A6 `Show/7` and `Hide/3`, and A7 `byref`.
10. [x] `src/il2cpp/hook/umamusume/HighSpeedSetting.rs` raises the game's own settings: it reads
    `StoryManager::GetMaxHighSpeedType/0`, writes story through `StoryManager::SaveHighSpeedType/1`
    and training through `ApplicationSettingSaveLoader::set_TrainingHighSpeedType/1`, gated on
    `high_speed_settings` (default false). Remaining on this item:
    - [ ] the numeric members of `Gallop.StoryTimelineController.HighSpeedType` are unknown because
      the enum class is not in the dump, so the `HighSpeedSetting: max ...` log line is the only
      source of truth for the first run
    - [ ] `StoryManager::ChangeAutoHighSpeedSettingAndSave/1` and `ChangeSavedAutoHighSpeedSetting/0`
      are not called; they may cycle rather than set, so they need a probe before use
    - [ ] `StoryManager::get_IsHighSpeedMode/0 -> static bool` is not hooked
    - [ ] `StoryTimelineController::SetHighSpeedFrameCount/2 -> void(class<StoryTimelineTextTrackData>, int)`
      and `/1 -> void(class<StoryTimelineTextClipData>)` are not hooked; only the `(int)` overload is
    - [ ] `high_speed_settings` is config file only, it is not in the Config Editor GUI yet
