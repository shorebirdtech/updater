import 'package:mocktail/mocktail.dart';
import 'package:shorebird_code_push/shorebird_code_push.dart';
import 'package:shorebird_code_push/src/updater.dart';
import 'package:test/test.dart';

class _MockUpdater extends Mock implements Updater {}

void main() {
  group(ShorebirdCodePushException, () {
    test('toString returns message prepended with class name', () {
      expect(
        ShorebirdCodePushException('message').toString(),
        'ShorebirdCodePushException: message',
      );
    });
  });

  group(ShorebirdCodePush, () {
    late Updater updater;
    Object? loggedError;

    setUp(() async {
      loggedError = null;
      updater = _MockUpdater();
    });

    group('initialize', () {
      test(
        '''returns an instance of ShorebirdCodePush if we can retrieve currentPatchNumber''',
        () {
          when(() => updater.currentPatchNumber()).thenReturn(0);

          expect(
            ShorebirdCodePush.initialize(buildUpdater: () => updater),
            completion(isNotNull),
          );
        },
      );

      test('returns null if we cannot retrieve currentPatchNumber', () {
        when(() => updater.currentPatchNumber()).thenThrow(ArgumentError());

        expect(
          ShorebirdCodePush.initialize(buildUpdater: () => updater),
          completion(isNull),
        );
      });
    });

    group('asdf', () {
      late ShorebirdCodePush shorebirdCodePush;

      setUp(() async {
        when(() => updater.currentPatchNumber()).thenReturn(0);
        shorebirdCodePush = (await ShorebirdCodePush.initialize(
          logError: (object) => loggedError = object,
          buildUpdater: () => updater,
        ))!;
      });

      group('isNewPatchAvailableForDownload', () {
        test('returns false if no update is available', () async {
          when(() => updater.checkForUpdate()).thenAnswer((_) => false);
          expect(
            await shorebirdCodePush.isNewPatchAvailableForDownload(),
            isFalse,
          );
          expect(loggedError, isNull);
        });

        test('returns true if an update is available', () async {
          when(() => updater.checkForUpdate()).thenAnswer((_) => true);
          expect(
            await shorebirdCodePush.isNewPatchAvailableForDownload(),
            true,
          );
          expect(loggedError, isNull);
        });

        test(
            '''throws a ShorebirdCodePushException if the updater throws an exception''',
            () async {
          when(() => updater.checkForUpdate()).thenThrow(Exception('oh no'));
          await expectLater(
            () async =>
                await shorebirdCodePush.isNewPatchAvailableForDownload(),
            throwsA(
              isA<ShorebirdCodePushException>().having(
                (exception) => exception.message,
                'message',
                '[ShorebirdCodePush] Exception: oh no',
              ),
            ),
          );
          expect(loggedError, '[ShorebirdCodePush] Exception: oh no');
        });

        test(
            '''returns false and logs an error if the updater raises an ArgumentError''',
            () async {
          when(() => updater.checkForUpdate()).thenThrow(ArgumentError());
          expect(
              await shorebirdCodePush.isNewPatchAvailableForDownload(), false);
          expect(
            loggedError,
            '''
[ShorebirdCodePush] Invalid argument(s)
  This is likely because you are not running with the Shorebird Flutter engine (that is, if you ran with `flutter run` instead of `shorebird run`).''',
          );
        });
      });

      group('currentPatchNumber', () {
        test('returns null if current patch is reported as 0', () async {
          when(() => updater.currentPatchNumber()).thenReturn(0);
          expect(await shorebirdCodePush.currentPatchNumber(), isNull);
          expect(loggedError, isNull);
        });

        test(
            '''forwards the return value of updater.currentPatchNumber if the patch number is >1''',
            () async {
          when(() => updater.currentPatchNumber()).thenReturn(1);
          expect(await shorebirdCodePush.currentPatchNumber(), 1);
          expect(loggedError, isNull);
        });

        test(
            '''throws a ShorebirdCodePushException if the updater throws an exception''',
            () async {
          when(() => updater.currentPatchNumber())
              .thenThrow(Exception('oh no'));
          await expectLater(
            () async => await shorebirdCodePush.currentPatchNumber(),
            throwsA(
              isA<ShorebirdCodePushException>().having(
                (exception) => exception.message,
                'message',
                '[ShorebirdCodePush] Exception: oh no',
              ),
            ),
          );
          expect(loggedError, '[ShorebirdCodePush] Exception: oh no');
        });

        test(
            '''returns null and logs an error if the updater raises an ArgumentError''',
            () async {
          when(() => updater.currentPatchNumber()).thenThrow(ArgumentError());
          expect(await shorebirdCodePush.currentPatchNumber(), null);
          expect(
            loggedError,
            '''
[ShorebirdCodePush] Invalid argument(s)
  This is likely because you are not running with the Shorebird Flutter engine (that is, if you ran with `flutter run` instead of `shorebird run`).''',
          );
        });
      });

      group('nextPatchNumber', () {
        test('returns null if next patch is reported as 0', () async {
          when(() => updater.nextPatchNumber()).thenReturn(0);
          expect(await shorebirdCodePush.nextPatchNumber(), isNull);
          expect(loggedError, isNull);
        });

        test(
            '''forwards the return value of updater.nextPatchNumber if the patch number is >1''',
            () async {
          when(() => updater.nextPatchNumber()).thenReturn(1);
          expect(await shorebirdCodePush.nextPatchNumber(), 1);
          expect(loggedError, isNull);
        });

        test(
            '''throws a ShorebirdCodePushException if the updater throws an exception''',
            () async {
          when(() => updater.nextPatchNumber()).thenThrow(Exception('oh no'));
          await expectLater(
            () async => shorebirdCodePush.nextPatchNumber(),
            throwsA(
              isA<ShorebirdCodePushException>().having(
                (exception) => exception.message,
                'message',
                '[ShorebirdCodePush] Exception: oh no',
              ),
            ),
          );
          expect(loggedError, '[ShorebirdCodePush] Exception: oh no');
        });

        test(
            '''returns null and logs an error if the updater raises an ArgumentError''',
            () async {
          when(() => updater.nextPatchNumber()).thenThrow(ArgumentError());
          expect(await shorebirdCodePush.nextPatchNumber(), isNull);
          expect(
            loggedError,
            '''
[ShorebirdCodePush] Invalid argument(s)
  This is likely because you are not running with the Shorebird Flutter engine (that is, if you ran with `flutter run` instead of `shorebird run`).''',
          );
        });
      });

      group('downloadUpdate', () {
        test('completes successfully if updater does not raise an exception',
            () async {
          when(() => updater.downloadUpdate()).thenReturn(null);
          await expectLater(
            shorebirdCodePush.downloadUpdateIfAvailable(),
            completes,
          );
          expect(loggedError, isNull);
        });

        test(
            '''throws a ShorebirdCodePushException if the updater throws an exception''',
            () async {
          when(() => updater.downloadUpdate()).thenThrow(Exception('oh no'));
          await expectLater(
            () async => shorebirdCodePush.downloadUpdateIfAvailable(),
            throwsA(
              isA<ShorebirdCodePushException>().having(
                (exception) => exception.message,
                'message',
                '[ShorebirdCodePush] Exception: oh no',
              ),
            ),
          );
          expect(loggedError, '[ShorebirdCodePush] Exception: oh no');
        });

        test(
            'completes and logs an error if the updater raises an ArgumentError',
            () async {
          when(() => updater.downloadUpdate()).thenThrow(ArgumentError());
          await expectLater(
            shorebirdCodePush.downloadUpdateIfAvailable(),
            completes,
          );
          expect(
            loggedError,
            '''
[ShorebirdCodePush] Invalid argument(s)
  This is likely because you are not running with the Shorebird Flutter engine (that is, if you ran with `flutter run` instead of `shorebird run`).''',
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
  });
}
