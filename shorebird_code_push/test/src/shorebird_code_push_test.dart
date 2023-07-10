// ignore_for_file: prefer_const_constructors

import 'package:mocktail/mocktail.dart';
import 'package:shorebird_code_push/shorebird_code_push.dart';
import 'package:shorebird_code_push/src/shorebird_code_push_ffi.dart';
import 'package:shorebird_code_push/src/updater.dart';
import 'package:test/test.dart';

class _MockUpdater extends Mock implements Updater {}

void main() {
  group('ShorebirdCodePushFfi', () {
    late Updater updater;
    late ShorebirdCodePush shorebirdCodePush;

    setUp(() {
      updater = _MockUpdater();
      shorebirdCodePush = ShorebirdCodePushFfi.forTest(
        buildUpdater: () => updater,
      );
    });

    group('isNewPatchAvailableForDownload', () {
      test('returns false if no update is available', () async {
        when(() => updater.checkForUpdate()).thenAnswer((_) => false);
        expect(
          await shorebirdCodePush.isNewPatchAvailableForDownload(),
          isFalse,
        );
      });

      test('returns true if an update is available', () async {
        when(() => updater.checkForUpdate()).thenAnswer((_) => true);
        expect(await shorebirdCodePush.isNewPatchAvailableForDownload(), true);
      });

      test('returns false if updater throws exception', () async {
        when(() => updater.checkForUpdate()).thenThrow(Exception('oh no'));
        expect(
          await shorebirdCodePush.isNewPatchAvailableForDownload(),
          isFalse,
        );
      });
    });

    group('currentPatchNumber', () {
      test('returns null if current patch is reported as 0', () async {
        when(() => updater.currentPatchNumber()).thenReturn(0);
        expect(await shorebirdCodePush.currentPatchNumber(), isNull);
      });

      test('forwards the return value of updater.currentPatchNumber', () async {
        when(() => updater.currentPatchNumber()).thenReturn(1);
        expect(await shorebirdCodePush.currentPatchNumber(), 1);
      });

      test('returns null if updater throws exception', () async {
        when(() => updater.currentPatchNumber()).thenThrow(Exception('oh no'));
        expect(await shorebirdCodePush.currentPatchNumber(), isNull);
      });
    });

    group('nextPatchNumber', () {
      test('returns null if current patch is reported as 0', () async {
        when(() => updater.nextPatchNumber()).thenReturn(0);
        expect(await shorebirdCodePush.nextPatchNumber(), isNull);
      });

      test('forwards the return value of updater.nextPatchNumber', () async {
        when(() => updater.nextPatchNumber()).thenReturn(1);
        expect(await shorebirdCodePush.nextPatchNumber(), 1);
      });

      test('returns null if updater throws exception', () async {
        when(() => updater.nextPatchNumber()).thenThrow(Exception('oh no'));
        expect(await shorebirdCodePush.nextPatchNumber(), isNull);
      });
    });

    group('downloadUpdate', () {
      test('forwards the return value of updater.nextPatchNumber', () async {
        when(() => updater.downloadUpdate()).thenReturn(null);
        await expectLater(
          shorebirdCodePush.downloadUpdateIfAvailable(),
          completes,
        );
      });

      test('logs error if updater throws exception', () async {
        when(() => updater.downloadUpdate()).thenThrow(Exception('oh no'));
        await expectLater(
          shorebirdCodePush.downloadUpdateIfAvailable(),
          completes,
        );
      });
    });

    group('isNewPatchReadyToInstall', () {
      test('returns false is no new patch is available', () async {
        when(() => updater.currentPatchNumber()).thenReturn(1);
        when(() => updater.nextPatchNumber()).thenReturn(0);
        expect(await shorebirdCodePush.isNewPatchReadyToInstall(), isFalse);
      });

      test(
        'returns false is the next patch is the same as the current patch',
        () async {
          when(() => updater.currentPatchNumber()).thenReturn(1);
          when(() => updater.nextPatchNumber()).thenReturn(1);
          expect(await shorebirdCodePush.isNewPatchReadyToInstall(), isFalse);
        },
      );

      test(
          '''returns true if the next patch number is greater than the current patch number''',
          () async {
        when(() => updater.currentPatchNumber()).thenReturn(1);
        when(() => updater.nextPatchNumber()).thenReturn(2);
        expect(await shorebirdCodePush.isNewPatchReadyToInstall(), isTrue);
      });
    });
  });
}
