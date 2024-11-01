import 'dart:ffi';

import 'package:ffi/ffi.dart';
import 'package:mocktail/mocktail.dart';
import 'package:shorebird_code_push/src/generated/updater_bindings.g.dart';
import 'package:shorebird_code_push/src/updater.dart';
import 'package:test/test.dart';

class _MockUpdaterBindings extends Mock implements UpdaterBindings {}

void main() {
  group(Updater, () {
    late UpdaterBindings updaterBindings;
    late Updater updater;

    setUpAll(() {
      registerFallbackValue(Pointer.fromAddress(0));
    });

    setUp(() {
      updaterBindings = _MockUpdaterBindings();

      updater = const Updater();
      Updater.bindings = updaterBindings;
    });

    test('initializes from currently loaded library', () {
      expect(updater, isNotNull);
    });

    group('currentPatchNumber', () {
      test('forwards the result of shorebird_next_boot_patch_number', () {
        when(
          () => updaterBindings.shorebird_current_boot_patch_number(),
        ).thenReturn(123);
        final currentPatchNumber = updater.currentPatchNumber();
        expect(currentPatchNumber, 123);
      });
    });

    group('checkForUpdate', () {
      test('forwards the result of shorebird_check_for_update', () {
        when(
          () => updaterBindings.shorebird_check_for_update(),
        ).thenReturn(true);
        expect(updater.checkForUpdate(), isTrue);

        when(
          () => updaterBindings.shorebird_check_for_update(),
        ).thenReturn(false);
        expect(updater.checkForUpdate(), isFalse);
      });
    });

    group('nextPatchNumber', () {
      test('forwards the result of shorebird_next_boot_patch_number', () {
        when(
          () => updaterBindings.shorebird_next_boot_patch_number(),
        ).thenReturn(123);
        final currentPatchNumber = updater.nextPatchNumber();
        expect(currentPatchNumber, 123);
      });
    });

    group('downloadUpdate', () {
      test('calls bindings.shorebird_update', () {
        when(() => updaterBindings.shorebird_update()).thenReturn(null);
        updater.downloadUpdate();
        verify(() => updaterBindings.shorebird_update()).called(1);
      });
    });

    group('update', () {
      test('calls bindings.shorebird_update_with_result', () {
        when(
          () => updaterBindings.shorebird_update_with_result(),
        ).thenReturn(nullptr);
        updater.update();
        verify(() => updaterBindings.shorebird_update_with_result()).called(1);
      });
    });

    group('freeUpdateResult', () {
      test('calls bindings.shorebird_free_update_result', () {
        final result = calloc.allocate<UpdateResult>(sizeOf<UpdateResult>());
        updater.freeUpdateResult(result);
        verify(
          () => updaterBindings.shorebird_free_update_result(any()),
        ).called(1);
      });
    });
  });
}
