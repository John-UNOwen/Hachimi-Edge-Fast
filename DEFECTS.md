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

- [x] **A1 `NowLoading::PlayFadeNowLoading` scales the wrong arguments.** 31 calls were logged
  as `NowLoading::PlayFadeNowLoading(0, 1, 0.3)` x15 and `(1, 0, 0.3)` x16. Arguments 0 and 1
  are the fade endpoints (alpha to and alpha from), argument 2 is the duration. The hook in
  `src/il2cpp/hook/umamusume/NowLoading.rs` scaled every float it receives, so at factor 20 the
  target alpha became 0.05 instead of 1. Fixed in `db3c272`, index 2 only is now scaled.
- [~] **A2 Overload matching is too coarse.** `symbols::get_method_overload` compares
  `Il2CppTypeEnum` only, and `CLASS` matches every reference type. The dump shows
  `SingleModeResultContentBase::FadeInContent/4` is not a duplicate pair but distinct overloads
  taking `UnityEngine.UI.MaskableGraphic` versus `UnityEngine.CanvasGroup`, and
  `FadeInContentFromBottom/4` takes `CanvasGroup` versus `Gallop.TextCommon`. Partly worked
  around: `SingleModeResultContentBase.rs` resolves its three fades through
  `AnimationSpeed::resolve_method` with an explicit parameter type list, and those hooks fired 16
  times in the 07:53 run. The coarse matcher itself is still in place for every other caller.
- [ ] **A3 Static helpers are currently not hooked.** The new static guard rejected two targets:
  `StoryTimelineController::GetTimeScaleByHighSpeedType/1 -> static float(bool)` and
  `GetTimeScaleHighSpeed/1 -> static float(bool)`. The guard prevented a wrapper that reserves a
  register for `this` from reading the `bool` from the wrong register. `StoryViewController::GetTimeScaleByHighSpeedType/0 -> static float()` confirms the existing
  no-`this` wrapper in `StoryViewController.rs` is correct. Fix: add argument-only wrappers for
  static targets. The matcher half is done: `AnimationSpeed::resolve_static_method` now matches the
  dumped parameter types and return type and *requires* the static flag, and four probes in
  `StoryFrameProbe.rs` are installed through it. What remains is rewriting these two as argument-only
  wrappers, which is the cheapest remaining story scale lever because both return the scale the story
  timeline then multiplies `deltaTime` by.
- [ ] **A4 Only 2 of 13 installed scaling points were reached.** Fired:
  `CountupModifier_getDuration 0.16 -> 0.008` and
  `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5`. Installed with no call in this session:
  `TextModifier_getDuration`, `TextModifier_getDelay`, `TrainingFooter_GetCloseAnimWaitTime`,
  `TrainingFooter_GetItemAnimDuration`, `TrainingCuttClip_getDelayTime`,
  `SingleModeUtils_GetHighSpeedPlayDuration`, `SingleModeUtils_GetCutTimeScale`,
  `StoryTimeline_getTimeScaleEventWipe`, `TeamStadiumGrandResult_FadeInContentFromRight`,
  `FadeInContent`, `FadeInContentFromRight`, `FadeInContentFromBottom`,
  `SetHighSpeedFrameCount/1 (int)`, `ActivateSkipButton`, `PlayInNowLoading`, `PlayOutNowLoading`.
  Run 2 retires part of this list: `FadeInContentFromRight` reached the game 16 times.
  `ActivateSkipButton` now logs every call (`b138a9b`), so a run can tell an installed hook from one
  the game actually used.
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
- [~] **A7 `byref` parameters are ignored when matching, but are now verified before use.**
  `symbols::get_method_overload` still compares `Il2CppTypeEnum` only, and this client keeps the
  element type in the enum: the dumped `float&` and `int&` report R4 and I4, with the reference held
  in a separate bit of `Il2CppType`. `AnimationSpeed::resolve_ref_method` now checks that bit on
  every parameter of the resolved overload before a wrapper may write through it, and
  `StoryTimelineController::GetNextFrameCount_HighSpeed` is hooked through it (`be41b21`). A
  reference parameter and a value parameter of the same type are still indistinguishable to the
  matcher.
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
min 326 ms, median 1024 ms, max 3177 ms, total 18.9 s. `ui_animation_scale` was 1.0 in this run, so
the fades were not pre compressed. Any future claim of speedup must be measured against these
numbers, not against feel.

### Run 2, build `3adb2bf-dirty`, 07:53:03 to 08:00:52, 469.5 s

`hachimi.log` 277 KB / 2609 lines. 198 hook installs, 10 `_addr is null`, 22 name resolution
warnings, 2 static rejections, 0 panics. Deployed build after the fixes below: `db3c272`,
40,155,648 bytes, SHA256 `191F9B24A2666F105578FC9DD8A31D924E786910CB9E7A3772FE1B48A12E7E1B`.

- 47 wipe calls formed 23 in transition pairs: gaps in ms 980, 1022, 697, 19, 987, 676, 303, 599,
  2072, 377, 1177, 1146, 1185, 3657, 222, 969, 2137, 215, 2095, 604, 412, 1343, 959. min 19 ms,
  median 969 ms, mean 1037 ms, max 3657 ms, total 23.9 s, which is 5.1% of the session. The 19 ms
  pair shows the scaled fades themselves cost nothing once the target assets are already loaded.
- 22 screen intervals (fade from black to the next fade to black): min 1.5 s, median 9.5 s,
  mean 19.5 s, max 83.7 s, total 428.6 s, which is 91% of the session. That time is spent on
  screens rather than in animation, so it is where any further gain has to come from.
- `SingleModeResultContentBase::FadeInContentFromRight` fired 16 times carrying the shipped
  durations 0.03, 0.06, 0.09 and 0.12 s. `CountupModifier_getDuration 0.16 -> 0.008` and
  `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` fired again. `PlayInNowLoading`,
  `PlayOutNowLoading`, `SetupLoadingTips`, `getTimeScaleEventWipe`, `SetHighSpeedFrameCount` and
  `ActivateSkipButton` still appear only as install lines, never as calls.

### Run 3, career run, build `6333285` (merged upstream v0.32.0), 08:29:44 to 08:36:15, 391.5 s

`hachimi.log` 105 KB / 909 lines. 200 hook installs, 190 armed in one pass in 0.067 s, 10 `_addr is
null`, 5 class not found, 23 `= NULL` resolutions, 2 static rejections, 0 panics, 0 arming failures.
`time_scale` was 2 in this run, and the config snapshot recorded it.

- 22 wipe calls formed 11 in transition pairs: gaps in ms 1056, 810, 1149, 937, 2374, 181, 966,
  1012, 246, 1009, 927. min 181 ms, median 966 ms, max 2374 ms, total 10.7 s, which is 2.7% of the
  session. Every call carried 0.3 s raw and every call had its counterpart.
- 10 screen intervals: min 1.4 s, median 27.0 s, mean 37.4 s, max 115.1 s, total 373.7 s, which is
  95% of the session. Three runs now agree that the wipe path is nearly exhausted and what is left
  is time spent on screens.
- `ActivateSkipButton (auto_skip_result_screens true)` fired 5 times with zero
  `SkipFadeInTween unavailable`, so the automatic result screen skip is confirmed end to end. 15
  `FadeInContentFromRight` calls carried 0.03, 0.033, 0.06, 0.066, 0.09, 0.099, 0.12 and 0.15 s raw.
- `HighSpeedSetting` logged 73 read snapshots and exactly one write, `story high speed 0 -> 2 ...
  saved now 2`, plus `training high speed 0 -> 2, read back 2`. A10 stays closed.
- `GetNextFrameCount_HighSpeed` and `SetHighSpeedFrameCount` were installed and never called, while
  `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` and `CountupModifier_getDuration 0.6 -> 0.03`
  prove story and result code did run. See item 13.

- [x] **A10 `HighSpeedSetting` re-applied the same write at every scene change.** 50 lines read
  `story high speed 1 -> 2 via StoryManager::SaveHighSpeedType, read back 1`. `SaveHighSpeedType`
  moves StoryManager's own saved setting, the `saved story` value did go 1 to 2, but
  `ApplicationSettingSaveLoader::get_StoryHighSpeedType` kept reporting 1 and the idempotency
  check used the loader value. Fixed in `db3c272` by comparing against `GetSavedHighSpeedSetting`.
- [ ] **A11 `GetMaxHighSpeedType` is context dependent.** Two snapshots in one session:
  `max 1, saved story 1, loader story 1, training 2` x27 at menus, and
  `max 2, saved story 2, loader story 1, training 2` x50 once story content was reached. The game
  ceiling is 2 in story context, the story setting was genuinely raised from 1 to 2, and training
  already sat at 2. A ceiling read in the wrong context raises nothing.
- [x] **A12 Hook installation costs about 4.6 s of every launch.** `Initializing il2cpp hooks` at
  07:53:03.332 to `Hooking finished` at 07:53:08.013, with `MH_EnableHook: MH_OK` events spaced at a
  uniform 24 ms. 192 enables at that spacing is 4.6 s. MinHook separates building a detour from
  arming it, so `Interceptor::begin_batch` and `finish_batch` create every hook during `hook::init`
  and arm them in one `MH_EnableHook(MH_ALL_HOOKS)` pass (`865935f`). A failed batch arms each
  target on its own rather than leaving the build unarmed. Android keeps arming at create time, so
  the batch is a no-op there. The line to read in the next run is
  `Hooking finished: N hooks armed in one pass, S s`, and S is what has to beat 4.68.
- [ ] **A13 The scaling direction of `GetNextFrameCount_HighSpeed` is not known yet.** The
  signature `void(float&, int&)` does not say which half is the wait between story characters and
  which half is the step the timeline advances. The wrapper divides both and never multiplies, so a
  value that turns out to be an index into the game's readonly `_highSpeedFrameCountArray` cannot be
  pushed past the end of it. The first six calls log `frames a -> b, count c -> d (story xN)`. The
  next run has to show whether story screens get shorter before the direction is settled.
- [x] **A14 A run did not record the settings it ran with.** `hook::init` now writes one
  `Config snapshot:` line carrying transition, result, story, ui_animation, time_scale, story_tcps,
  choice_delay, target_fps, auto_skip_result, high_speed_settings, hide_now_loading and the physics
  mode (`865935f`), which is what made run 2 ambiguous: `config.json` read 3.0 and 1.0 at 03:31 and
  1000.0 and 1000.0 at 04:00 with nothing in the log to say which values were live.

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
  process by image name with no ownership check. Batch arming (`865935f`) removes the 24 ms per hook
  from this list but not the dump, the sqlite hooking, or the window work.
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
- [latent] **C32 No wrapper in this tree declares the trailing `const MethodInfo*`** that IL2CPP
  generates as the last parameter of every managed method, so a detour that calls its trampoline
  leaves that slot holding whatever register value it had. Zero of the ~200 wrappers pass it
  (`grep` for a `MethodInfo` parameter returns nothing), which is the convention across the whole
  IL2CPP hooking ecosystem, and three logged sessions with 200 armed hooks reached training, races
  and result screens without a single related fault. Recorded as a known property, not a defect to
  chase: it only matters for a wrapper that would dereference method metadata, and none does.
- [ ] **C33 `get_orig_fn!` is a `Mutex<HashMap>` lookup with `unwrap()` per call**
  (`src/core/interceptor.rs:113-119`). Every detour pays it, and the observe only probes add fifteen
  more candidates on paths the story code walks per frame. Cost is tens of nanoseconds and is not
  what this fork is losing time to, but the `unwrap()` on a shared lock inside an `extern "C"` frame
  is the same shape C2 warns about. Cached trampoline handles are the fix if this ever lands on a
  measured hot path.

## D. Fix order

1. [x] A1 duration argument index in `NowLoading` (`db3c272`).
2. A2 overload matching by parameter class name.
3. A3 argument-only wrappers for static targets, then re-enable both high speed helpers.
4. A5 hook `PlayFadeFrontCanvas` and the `HighSpeedType` enum overload.
5. C2 unwind and SEH barrier at hook entry, `unwrap_or_else` on shared mutexes, bypass instead of
   `process::exit`.
6. C5 clamp `ui_animation_scale` in code and leave `independent_time` alone.
7. C22 and C24, the two compounding multipliers that are live in the current config.
8. C1 remaining `get_orig_fn!` audit and the proxy export stubs.
9. A6 `Show/7` and `Hide/3` still open. A7 is `[~]`: the reference bit is verified before a wrapper
   writes back and one method is hooked through it (`be41b21`), the matcher itself is unchanged.
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
    - [x] `high_speed_settings` is in the Config Editor, on the Performance tab (`e705585`, `3adb2bf`)

11. Read the next run before choosing the direction of A13: the line
    `Hooking finished: N hooks armed in one pass, S s` against the measured 4.68 s, the six
    `GetNextFrameCount_HighSpeed frames a -> b, count c -> d` lines, and whether the 428.6 s of screen
    time in run 2 moved.
12. The story text path is only reachable through the methods that read its constants.
    `StoryTimelineTextClipData.TYPEWRITER_WAIT_FRAME`, `StoryTimelineController.FADE_TIME_FOR_HIGH_SPEED`,
    `TOUCH_BLOCK_INTERVAL` and `CONTINUOUS_TOUCH_INTERVAL` all logged as compile-time constants in run 2,
    and `resolved 0/61 duration fields` still holds. A13 is the first of those methods.
13. The frame stepping path this client uses is still unidentified. `GetNextFrameCount_HighSpeed`
    and `SetHighSpeedFrameCount` are installed, signature verified, and never called in a 391 s
    career run that reached training turns, races and five result screens. [StoryFrameProbe.rs](src/il2cpp/hook/umamusume/StoryFrameProbe.rs)
    observes fifteen candidates with every argument handed to the original untouched:
    `SkipFrameCount`, `SetFrameCountForWaiting`, `SkipMotionFrame`, `IsSkipToTextClip`,
    `get_WaitFrameCountUntilNextBlock` and its setter, `get_WaitFrameUntilNextBlock` and its setter,
    `get_WaitingFrameCount`, `UpdateTimeScaleByHispeedType`, `get_TimeScale` and `set_TimeScale`,
    `IsHighSpeedMode`, `IsStoryEndFrameOrGrandLiveWaitFrameSkipped` and
    `StoryTimelineTextClipData::GetWaitFrameUntilNextBlockLocalize`. The read half and the write half
    are both probed because a busy getter measures polling while a busy setter measures advancement,
    and the `TimeScale` pair is who writes the value C22 compounds. Nothing is matched on name plus
    arity: every candidate goes through `AnimationSpeed::resolve_method` or the new
    `resolve_static_method`, both of which check the dumped parameter types, the return type,
    genericness, the static flag and whether each parameter is a reference. It installs only under
    debug_mode, logs the first six calls of each probe, prints `Frame probe totals at N s:` every
    20 s while the totals grow, and lists any candidate that never resolved at install time. A probe
    that never appears is itself the answer, and A13 can only be pointed in a direction once those
    totals exist.

## E. Merge with upstream v0.32.0 (`5f89a7e`)

Merge base `83dd99f` (upstream #231). 11 upstream commits, 38 files, 1845 insertions, 136 deletions.
One textual conflict, in `src/il2cpp/mod.rs`: upstream made `utils` public while we had added
`introspect` and kept `utils` private, resolved by taking both. Every other overlap (`gui.rs`,
`hachimi.rs`, seven locale files, `GameSystem.rs`, `SceneManager.rs`, `umamusume/mod.rs`,
`UnityEngine_CoreModule/mod.rs`) merged without intervention, and the merged tree builds with no
errors and no warnings.

What arrived:

- JP only shadow options `shadow_distance`, `soft_shadows`, `soft_shadow_quality`,
  `shadow_depth_bias`, `shadow_normal_bias`, `force_chara_shadows` and `story_shadow_type`, together
  with new modules `StoryTimelineBg3DClipData`, `CascadeShadow`, `CascadeShadowForRace`,
  `GallopRenderer`, `Shader` and `ScreenSpaceAmbientOcclusion`. They are region gated to Japan, so on
  this Global client they are inert.
- Standalone custom font support: `custom_font_file` replaces `custom_font_asset_bundle` and
  `custom_font_name`, `.hachifont` ZIP packs are scanned every 3 s while the Config Editor is open,
  bundles load through `AssetBundle.LoadFromMemoryAsync_Internal`, with a 2 GB bundle limit, a 4 KB
  `font_path.txt` limit and a one time `custom_font_file_warning`.
- `SceneManager` caches `GetCurrentSceneId` from an `AlterUpdate` detour and exposes
  `is_race_scene_family()`. Its new wrapper inherits the zero address guard our macro family already
  applies. Our speed groups stay installed last in `umamusume::init`.
- `GameSystem` writes `_isMSAA` when msaa is enabled, the race playback slider and button no longer
  persist as invisible unclickable areas after a race, and race finish detection for overlays is fixed.
- `tl_repo::download_incremental` writes to a `.part` file, localized data reloads on an update
  failure, corrupted texture diffs are deleted in favour of the original game texture, and
  `run_internal` returns `RuntimeError` instead of panicking on an unset TL repo directory or id.

- [ ] **E1 Upstream locale duplicates.** `ko.yml` defines `shadow_depth_bias` and `shadow_normal_bias`
  twice inside `config_editor`, `zh-cn.yml` defines `custom_font_file` and `custom_font_none_found`
  twice. The earlier definition is never read.
- [ ] **E2 The new option labels are untranslated in the locales this fork maintains.** `es`, `id` and
  `vi` carry none of the nine new keys, `fil` is missing four shadow keys and `zh-tw` is missing
  `custom_font_file_warning`. They fall back to English. `performance_tab` is present in all ten.
- A6, A13, C22, C23 and C24 are untouched by this merge. Upstream changed nothing in `NowLoading.rs`,
  and its `StoryTimelineData.rs` edit is the JP `rewrite_story_shadow_types` path.
