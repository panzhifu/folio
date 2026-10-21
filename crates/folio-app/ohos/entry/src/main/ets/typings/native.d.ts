// Type declarations for the Folio native module (`libfolio_app.so`).
//
// The module is registered from Rust via `gpui_ohos_napi_module!` in
// `src/ui/platform/ohos.rs`; the exports below mirror the
// `napi_create_function` registrations in `gpui-mobile/src/ohos/napi.rs`.
declare module 'libfolio_app.so' {
  export interface FolioNative {
    /** Force the module to load / initialise (no-op on the native side). */
    nativeInit(): void;
    /** Display density relative to 160 dpi (densityDPI / 160). */
    nativeSetDensity(scale: number): void;
    /** Window safe-area insets in vp: top, bottom, left, right. */
    nativeSetSafeAreaInsets(top: number, bottom: number, left: number, right: number): void;
    /** App moved to the foreground. */
    nativeOnForeground(): void;
    /** App moved to the background. */
    nativeOnBackground(): void;
    /** App is being destroyed. */
    nativeOnDestroy(): void;
    /**
     * Register the software-keyboard bridge. The callback receives a
     * keyboard-type string to show the IME, or `undefined` to hide it.
     */
    nativeSetKeyboardCallback(cb: (kind?: string) => void): void;
  }

  const native: FolioNative;
  export default native;
}
