import 'dart:ffi' as ffi;

import 'package:ffi/ffi.dart';
import 'package:meta/meta.dart';
import 'package:path/path.dart' as p;
import 'package:platform/platform.dart';
import 'package:shorebird_code_push/src/generated/updater_bindings.g.dart';

/// {@template updater}
/// A wrapper around the generated [UpdaterBindings] that translates ffi types
/// into easier to use Dart types.
/// {@endtemplate}
class Updater {
  /// Creates an [Updater] instance using the currently loaded dynamic library.
  Updater.init() {
    bindings = UpdaterBindings(ffi.DynamicLibrary.process());
  }

  /// Creates an [Updater] instance using the dynamic library at the given
  /// [directory].
  ///
  /// The name of the library is determined by the current OS:
  ///   macOS:   lib[name].dylib
  ///   Windows: [name].dll
  ///   Linux:   lib[name].so
  Updater.initWithLibrary({
    required String directory,
    required String name,
    ffi.DynamicLibrary Function(String) ffiOpen =
        ffi.DynamicLibrary.open, // for testing
  }) {
    final dylib = () {
      if (platform.isMacOS) {
        return ffiOpen(p.join(directory, 'lib$name.dylib'));
      }
      if (platform.isWindows) {
        return ffiOpen(p.join(directory, '$name.dll'));
      }
      // Assume everything else follows the Linux pattern.
      return ffiOpen(p.join(directory, 'lib$name.so'));
    }();
    bindings = UpdaterBindings(dylib);
  }

  /// The ffi bindings to the Updater library.
  @visibleForTesting
  static late UpdaterBindings bindings;

  /// An override for dart:io's [Platform] class for testing.
  @visibleForTesting
  static Platform platform = const LocalPlatform();

  /// The currently active patch number.
  // TODO(bryanoltman): this will return the current number + 1 if an update is
  // available. It should instead always return the current patch version.
  int? currentPatchNumber() {
    final patchNumberString = _returnsMaybeString(
      bindings.shorebird_next_boot_patch_number,
    );
    return patchNumberString == null ? null : int.tryParse(patchNumberString);
  }

  /// Whether a new patch is available.
  bool checkForUpdate() => bindings.shorebird_check_for_update();
  String? _returnsMaybeString(ffi.Pointer<ffi.Char> Function() f) {
    var cString = ffi.Pointer<ffi.Char>.fromAddress(0);
    cString = f();
    if (cString.address == ffi.nullptr.address) {
      return null;
    }

    final utf8Pointer = cString.cast<Utf8>();

    try {
      return utf8Pointer.toDartString();
    } finally {
      // Using finally for two reasons:
      // 1. it runs after the return (saving us a local)
      // 2. it runs even if toDartString throws (which it shouldn't)
      bindings.shorebird_free_string(cString);
    }
  }
  // TODO(bryanoltman): reintroduce this when working on Dart command line.
  // This is only used when called from a Dart command line.
  // Shorebird will have initialized the library already for you when
  // inside a Flutter app.
  // static void initUpdaterLibrary({
  //   required String appId,
  //   required String version,
  //   required String channel,
  //   required String? updateUrl,
  //   required List<String> baseLibraryPaths,
  //   required String vmPath,
  //   required String cacheDir,
  // }) {
  //   var config = AppParameters.allocate(
  //     appId: appId,
  //     version: version,
  //     channel: channel,
  //     updateUrl: updateUrl,
  //     libappPaths: baseLibraryPaths,
  //     libflutterPath: vmPath,
  //     cacheDir: cacheDir,
  //   );
  //   try {
  //     bindings.init(config);
  //   } finally {
  //     AppParameters.free(config);
  //   }
  // }
}
