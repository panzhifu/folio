import native from 'libfolio_app.so';
import { picker, fileUri, Environment, fileIo } from '@kit.CoreFileKit';
import { deviceInfo } from '@kit.BasicServicesKit';
import { hilog } from '@kit.PerformanceAnalysisKit';

const DOMAIN: number = 0x0000;
const TAG: string = 'Folio';

/// Separator the native side splits picked paths on (`U+0001`; a path may contain
/// almost anything else).
const PICK_SEP: string = '\u0001';

/// `file://docs/storage/Users/currentUser/Documents` → `/storage/Users/currentUser/Documents`.
///
/// The picker hands back URIs, but GPUI's `prompt_for_paths` and folio's library code
/// want a path the sandbox can actually read with `std::fs`. `FileUri.path` is the
/// official mapping — the URI's own path component *is* the sandbox path under the
/// app's mount namespace; this SDK has no `fs.realpath`.
function realPathOf(uri: string): string {
  return new fileUri.FileUri(uri).path;
}

/// Root page. The `XComponent` of type `SURFACE` with `libraryname: 'folio_app'`
/// makes the runtime load our native module (`libfolio_app.so`) and bind it to
/// this component. The native `OnSurfaceCreated` callback then receives the
/// `OHNativeWindow` that GPUI renders into.
@Entry
@Component
struct Index {
  private xcomponentId: string = 'gpui_surface';

  aboutToAppear(): void {
    // Register the software-keyboard bridge.  Native calls this with a
    // keyboard-type string to show the IME, or with no argument to hide it.
    //
    // NOTE: driving the OHOS IME needs a focused TextInput / the
    // `@ohos.inputMethod` API; that wiring is best-effort and currently just
    // logs. Hardware / XComponent key events already reach GPUI.
    try {
      native.nativeSetKeyboardCallback((kind?: string) => {
        hilog.info(DOMAIN, TAG, 'keyboard request: %{public}s', kind ?? 'hide');
      });
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetKeyboardCallback failed');
    }

    // The file / folder picker. The NDK has none, so GPUI's `prompt_for_paths`
    // arrives here as `(requestId, mode)` and is answered by
    // `nativeDeliverPick` / `nativeFailPick`.
    try {
      native.nativeSetFolderPicker((requestId: number, mode: string) => {
        this.openPicker(requestId, mode);
      });
      hilog.info(DOMAIN, TAG, 'folder picker registered');
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'nativeSetFolderPicker failed');
    }
  }

  private openPicker(requestId: number, mode: string): void {
    const options: picker.DocumentSelectOptions = new picker.DocumentSelectOptions();
    options.maxSelectNumber = mode === 'files' || mode === 'mixed' ? 20 : 1;
    options.selectMode = mode === 'folders'
      ? picker.DocumentSelectMode.FOLDER
      : (mode === 'mixed' ? picker.DocumentSelectMode.MIXED : picker.DocumentSelectMode.FILE);
    // Folder selection is gated on an *optional* system capability, and Huawei's own
    // guidance is that only PC / 2-in-1 / TV honour `FOLDER` — elsewhere the picker
    // degrades to "folders are navigation only", which looks exactly like a broken
    // bridge. Log the runtime verdict so the two are tellable apart.
    hilog.info(
      DOMAIN, TAG,
      'openPicker mode=%{public}s type=%{public}s folderCap=%{public}s folderObtain=%{public}s',
      mode,
      deviceInfo.deviceType,
      String(canIUse('SystemCapability.FileManagement.UserFileService.FolderSelection')),
      String(canIUse('SystemCapability.FileManagement.File.Environment.FolderObtain')));
    try {
      const dir: string = Environment.getUserDownloadDir();
      const probe: string = dir + '/folio-probe';
      if (fileIo.accessSync(probe)) {
        fileIo.rmdirSync(probe);
      }
      fileIo.mkdirSync(probe);
      const file: string = probe + '/x.txt';
      const handle = fileIo.openSync(file, fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE);
      fileIo.writeSync(handle.fd, 'probe');
      fileIo.closeSync(handle);
      fileIo.unlinkSync(file);
      fileIo.rmdirSync(probe);
      hilog.info(DOMAIN, TAG, 'downloadDir writable: %{public}s', dir);
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'downloadDir probe failed: %{public}s', JSON.stringify(e));
    }

    new picker.DocumentViewPicker()
      .select(options)
      .then((uris: string[]) => {
        if (!uris || uris.length === 0) {
          native.nativeDeliverPick(requestId, '');
          return;
        }
        const paths: string[] = [];
        for (const uri of uris) {
          const path: string = realPathOf(uri);
          // Only the user-file mount is reachable as a path; a media-library URI
          // (`file://media/…`) is fd-access only, so say so instead of handing back
          // something `std::fs` will fail on later.
          if (!path.startsWith('/storage/')) {
            hilog.warn(DOMAIN, TAG, 'unusable pick: %{public}s', uri);
            native.nativeFailPick(requestId, `no real path for ${uri}`);
            return;
          }
          paths.push(path);
        }
        hilog.info(DOMAIN, TAG, 'picked %{public}d folder(s)', paths.length);
        native.nativeDeliverPick(requestId, paths.join(PICK_SEP));
      })
      .catch((err: Error) => {
        // Backing out of the picker arrives as a rejection; treat it as a cancel but
        // keep the reason on the log so a real failure is not invisible.
        hilog.warn(DOMAIN, TAG, 'picker rejected: %{public}s', err?.message ?? 'unknown');
        native.nativeDeliverPick(requestId, '');
      });
  }

  build() {
    Column() {
      XComponent({
        id: this.xcomponentId,
        type: XComponentType.SURFACE,
        libraryname: 'folio_app'
      })
        .onLoad(() => {
          try {
            native.nativeInit();
          } catch (e) {
            hilog.warn(DOMAIN, TAG, 'nativeInit failed');
          }
        })
        .width('100%')
        .height('100%')
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#121318')
  }
}
