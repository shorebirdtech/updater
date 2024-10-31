import 'dart:async';

import 'package:mocktail/mocktail.dart';
import 'package:shorebird_code_push/src/shorebird_updater.dart';
import 'package:shorebird_code_push/src/shorebird_updater_io.dart';
import 'package:shorebird_code_push/src/updater.dart';
import 'package:test/test.dart';

import '../override_print.dart';

class _MockUpdater extends Mock implements Updater {}

Future<R> run<R>(
  FutureOr<R> Function() computation, {
  String? debugName,
}) async {
  return computation();
}

void main() {
  group(ShorebirdUpdaterImpl, () {
    late Updater updater;
    late ShorebirdUpdaterImpl shorebirdUpdater;

    setUp(() {
      updater = _MockUpdater();
    });

    group('isAvailable', () {
      group('when updater is available', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(1);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('returns true', () {
          expect(shorebirdUpdater.isAvailable, isTrue);
        });
      });

      group('when updater is unavailable', () {
        setUp(() {
          when(updater.currentPatchNumber).thenThrow(Exception('oops'));
        });

        test(
          'returns false',
          overridePrint((_) {
            shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
            expect(shorebirdUpdater.isAvailable, isFalse);
          }),
        );
      });
    });

    group('readPatch', () {
      group('when updater is unavailable', () {
        setUp(() {
          when(updater.currentPatchNumber).thenThrow(Exception('oops'));
        });

        test(
          'returns null',
          overridePrint((_) async {
            shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
            await expectLater(
              shorebirdUpdater.readCurrentPatch(),
              completion(isNull),
            );
            await expectLater(
              shorebirdUpdater.readNextPatch(),
              completion(isNull),
            );
          }),
        );
      });

      group('when updater has no installed patches', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          when(updater.nextPatchNumber).thenReturn(0);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('returns null', () async {
          await expectLater(
            shorebirdUpdater.readCurrentPatch(),
            completion(isNull),
          );
          await expectLater(
            shorebirdUpdater.readNextPatch(),
            completion(isNull),
          );
        });
      });

      group('when updater has a downloaded patch', () {
        const currentPatchNumber = 0;
        const nextPatchNumber = 1;
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(currentPatchNumber);
          when(updater.nextPatchNumber).thenReturn(nextPatchNumber);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('returns correct patch numbers', () async {
          await expectLater(
            shorebirdUpdater.readCurrentPatch(),
            completion(isNull),
          );
          await expectLater(
            shorebirdUpdater.readNextPatch(),
            completion(
              isA<Patch>().having(
                (p) => p.number,
                'number',
                nextPatchNumber,
              ),
            ),
          );
        });
      });

      group('when updater has an installed patch', () {
        const currentPatchNumber = 1;
        const nextPatchNumber = 1;
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(currentPatchNumber);
          when(updater.nextPatchNumber).thenReturn(nextPatchNumber);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('returns correct patch numbers', () async {
          await expectLater(
            shorebirdUpdater.readCurrentPatch(),
            completion(
              isA<Patch>().having(
                (p) => p.number,
                'number',
                currentPatchNumber,
              ),
            ),
          );
          await expectLater(
            shorebirdUpdater.readNextPatch(),
            completion(
              isA<Patch>().having(
                (p) => p.number,
                'number',
                nextPatchNumber,
              ),
            ),
          );
        });
      });

      group(
          'when updater has an installed patch '
          'and a new downloaded patch', () {
        const currentPatchNumber = 1;
        const nextPatchNumber = 2;
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(currentPatchNumber);
          when(updater.nextPatchNumber).thenReturn(nextPatchNumber);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('returns correct patch numbers', () async {
          await expectLater(
            shorebirdUpdater.readCurrentPatch(),
            completion(
              isA<Patch>().having(
                (p) => p.number,
                'number',
                currentPatchNumber,
              ),
            ),
          );
          await expectLater(
            shorebirdUpdater.readNextPatch(),
            completion(
              isA<Patch>().having(
                (p) => p.number,
                'number',
                nextPatchNumber,
              ),
            ),
          );
        });
      });

      group('when an exception occurs trying to read patches', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          when(updater.nextPatchNumber).thenThrow(Exception('oops'));
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('throws $UpdaterException', () async {
          await expectLater(
            () => shorebirdUpdater.readNextPatch(),
            throwsA(isA<UpdaterException>()),
          );
        });
      });
    });

    group('checkForUpdate', () {
      group('when updater is unavailable', () {
        setUp(() {
          when(updater.currentPatchNumber).thenThrow(Exception('oops'));
        });

        test(
          'returns UpdateStatus.unavailable',
          overridePrint((_) async {
            shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
            await expectLater(
              shorebirdUpdater.checkForUpdate(),
              completion(equals(UpdateStatus.unavailable)),
            );
          }),
        );
      });

      group('when updater has an update available', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          when(updater.checkForUpdate).thenReturn(true);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('returns UpdateStatus.outdated', () async {
          await expectLater(
            shorebirdUpdater.checkForUpdate(),
            completion(equals(UpdateStatus.outdated)),
          );
        });
      });

      group('when updater has downloaded an update', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          when(updater.nextPatchNumber).thenReturn(1);
          when(updater.checkForUpdate).thenReturn(false);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('returns UpdateStatus.restartRequired', () async {
          await expectLater(
            shorebirdUpdater.checkForUpdate(),
            completion(equals(UpdateStatus.restartRequired)),
          );
        });
      });

      group('when updater installed an update and is up to date', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(1);
          when(updater.nextPatchNumber).thenReturn(1);
          when(updater.checkForUpdate).thenReturn(false);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('returns UpdateStatus.upToDate', () async {
          await expectLater(
            shorebirdUpdater.checkForUpdate(),
            completion(equals(UpdateStatus.upToDate)),
          );
        });
      });
    });

    group('update', () {
      group('when updater is unavailable', () {
        setUp(() {
          when(updater.currentPatchNumber).thenThrow(Exception('oops'));
        });

        test(
          'does nothing',
          overridePrint((_) async {
            shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
            await expectLater(shorebirdUpdater.update(), completes);
            verifyNever(updater.downloadUpdate);
          }),
        );
      });

      group('when no update is available', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          when(updater.checkForUpdate).thenReturn(false);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('throws $UpdaterException', () async {
          await expectLater(
            shorebirdUpdater.update,
            throwsA(isA<UpdaterException>()),
          );
          verifyNever(updater.downloadUpdate);
        });
      });

      group('when download fails', () {
        setUp(() {
          when(() => updater.currentPatchNumber()).thenReturn(0);
          when(() => updater.nextPatchNumber()).thenReturn(0);
          when(() => updater.checkForUpdate()).thenReturn(true);
          when(() => updater.downloadUpdate()).thenReturn(null);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('throws $UpdaterException', () async {
          await expectLater(
            shorebirdUpdater.update,
            throwsA(
              isA<UpdaterException>().having(
                (e) => e.message,
                'message',
                'Failed to download update.',
              ),
            ),
          );
          verify(updater.downloadUpdate).called(1);
        });
      });

      group('when download succeeds', () {
        setUp(() {
          final updateCheck = [true, false];
          when(updater.currentPatchNumber).thenReturn(0);
          when(updater.nextPatchNumber).thenReturn(1);
          when(
            updater.checkForUpdate,
          ).thenAnswer((_) => updateCheck.removeAt(0));
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('completes', () async {
          await expectLater(shorebirdUpdater.update(), completes);
          verify(updater.downloadUpdate).called(1);
        });
      });
    });
  });
}
