import 'dart:ffi' as ffi;

import 'package:ffi/ffi.dart';
import 'package:mocktail/mocktail.dart';
import 'package:platform/platform.dart';
import 'package:shorebird_code_push/src/generated/updater_bindings.g.dart';
import 'package:shorebird_code_push/src/updater.dart';
import 'package:test/test.dart';

class _MockPlatform extends Mock implements Platform {}

class _MockUpdaterBindings extends Mock implements UpdaterBindings {}

void main() {
  group(Updater, () {
    late Platform platform;
    late UpdaterBindings updaterBindings;
    late Updater updater;

    setUp(() {
      platform = _MockPlatform();
      updaterBindings = _MockUpdaterBindings();

      updater = Updater.init();
      Updater.bindings = updaterBindings;
      Updater.platform = platform;
    });

    test('initializes from currently loaded library', () {
      expect(updater, isNotNull);
    });

    group('initWithLibrary', () {
      const libDirectory = 'lib';
      const libName = 'MyLibrary';

      test('opens libName.dylib on macOS', () {
        when(() => platform.isMacOS).thenReturn(true);
        when(() => platform.isWindows).thenReturn(false);
        when(() => platform.isLinux).thenReturn(false);

        var capturedPath = '';
        final updater = Updater.initWithLibrary(
          directory: libDirectory,
          name: libName,
          ffiOpen: (path) {
            capturedPath = path;
            return ffi.DynamicLibrary.process();
          },
        );

        expect(updater, isNotNull);
        expect(capturedPath, 'lib/libMyLibrary.dylib');
      });

      test('opens name.dll on Windows', () {
        when(() => platform.isMacOS).thenReturn(false);
        when(() => platform.isWindows).thenReturn(true);
        when(() => platform.isLinux).thenReturn(false);

        var capturedPath = '';
        final updater = Updater.initWithLibrary(
          directory: libDirectory,
          name: libName,
          ffiOpen: (path) {
            capturedPath = path;
            return ffi.DynamicLibrary.process();
          },
        );

        expect(updater, isNotNull);
        expect(capturedPath, 'lib/MyLibrary.dll');
      });

      test('opens libName.so on Linux', () {
        when(() => platform.isMacOS).thenReturn(false);
        when(() => platform.isWindows).thenReturn(false);
        when(() => platform.isLinux).thenReturn(true);

        var capturedPath = '';
        final updater = Updater.initWithLibrary(
          directory: libDirectory,
          name: libName,
          ffiOpen: (path) {
            capturedPath = path;
            return ffi.DynamicLibrary.process();
          },
        );

        expect(updater, isNotNull);
        expect(capturedPath, 'lib/libMyLibrary.so');
      });
    });

    group('currentPatchNumber', () {
      test('returns null if bindings return null pointer', () {
        when(() => updaterBindings.shorebird_next_boot_patch_number())
            .thenReturn(ffi.nullptr);
        final currentPatchNumber = updater.currentPatchNumber();
        expect(currentPatchNumber, isNull);
      });

      test('returns number if bindings return non-null pointer', () {
        final charPtr = '123'.toNativeUtf8().cast<ffi.Char>();
        when(() => updaterBindings.shorebird_next_boot_patch_number())
            .thenReturn(charPtr);
        final currentPatchNumber = updater.currentPatchNumber();
        expect(currentPatchNumber, 123);
      });
    });

    group('checkForUpdate', () {
      test('forwards the result of shorebird_check_for_update', () {
        when(() => updaterBindings.shorebird_check_for_update())
            .thenReturn(true);
        expect(updater.checkForUpdate(), isTrue);

        when(() => updaterBindings.shorebird_check_for_update())
            .thenReturn(false);
        expect(updater.checkForUpdate(), isFalse);
      });
    });
  });
}
