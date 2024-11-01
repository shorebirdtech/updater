import 'dart:async';
import 'dart:ffi';

import 'package:ffi/ffi.dart';
import 'package:mocktail/mocktail.dart';
import 'package:shorebird_code_push/src/generated/updater_bindings.g.dart';
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

        test('throws $ReadPatchException', () async {
          await expectLater(
            () => shorebirdUpdater.readNextPatch(),
            throwsA(isA<ReadPatchException>()),
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

      group('when a nullptr result is returned', () {
        setUp(() {
          when(() => updater.currentPatchNumber()).thenReturn(0);
          when(() => updater.downloadUpdateWithError()).thenReturn(nullptr);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('throws $UpdateException', () async {
          await expectLater(
            shorebirdUpdater.update,
            throwsA(
              isA<UpdateException>().having(
                (e) => e.message,
                'message',
                'An unknown error occurred.',
              ),
            ),
          );
          verify(updater.downloadUpdateWithError).called(1);
        });
      });

      group('when no update is available', () {
        setUp(() {
          when(() => updater.currentPatchNumber()).thenReturn(0);
          final result = calloc.allocate<UpdateResult>(8);
          result.ref.status = 0; // SHOREBIRD_NO_UPDATE
          addTearDown(() => calloc.free(result));
          when(() => updater.downloadUpdateWithError()).thenReturn(result);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('throws $UpdateException', () async {
          await expectLater(
            shorebirdUpdater.update,
            throwsA(
              isA<UpdateException>().having(
                (e) => e.message,
                'message',
                'No update available.',
              ),
            ),
          );
          verify(updater.downloadUpdateWithError).called(1);
        });
      });

      group('when an error occurs during download', () {
        setUp(() {
          when(() => updater.currentPatchNumber()).thenReturn(0);
          final result = calloc.allocate<UpdateResult>(8);
          result.ref.status = 2; // SHOREBIRD_UPDATE_HAD_ERROR
          result.ref.message = 'oops'.toNativeUtf8().cast<Char>();
          addTearDown(() => calloc.free(result));
          when(() => updater.downloadUpdateWithError()).thenReturn(result);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('throws $UpdateException', () async {
          await expectLater(
            shorebirdUpdater.update,
            throwsA(
              isA<UpdateException>().having(
                (e) => e.message,
                'message',
                'An error occurred while downloading the patch: oops',
              ),
            ),
          );
          verify(updater.downloadUpdateWithError).called(1);
        });
      });

      group('when the downloaded patch is bad', () {
        setUp(() {
          when(() => updater.currentPatchNumber()).thenReturn(0);
          final result = calloc.allocate<UpdateResult>(8);
          result.ref.status = 3; // SHOREBIRD_UPDATE_IS_BAD_PATCH
          addTearDown(() => calloc.free(result));
          when(() => updater.downloadUpdateWithError()).thenReturn(result);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('throws $UpdateException', () async {
          await expectLater(
            shorebirdUpdater.update,
            throwsA(
              isA<UpdateException>().having(
                (e) => e.message,
                'message',
                'Update available but previously failed to install.',
              ),
            ),
          );
          verify(updater.downloadUpdateWithError).called(1);
        });
      });

      group('when an unknown error occurs', () {
        setUp(() {
          when(() => updater.currentPatchNumber()).thenReturn(0);
          final result = calloc.allocate<UpdateResult>(8);
          result.ref.status = 4; // SHOREBIRD_UPDATE_ERROR
          addTearDown(() => calloc.free(result));
          when(() => updater.downloadUpdateWithError()).thenReturn(result);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('throws $UpdateException', () async {
          await expectLater(
            shorebirdUpdater.update,
            throwsA(
              isA<UpdateException>().having(
                (e) => e.message,
                'message',
                'An unknown error occurred.',
              ),
            ),
          );
          verify(updater.downloadUpdateWithError).called(1);
        });
      });

      group('when an unsupported status code is returned', () {
        setUp(() {
          when(() => updater.currentPatchNumber()).thenReturn(0);
          final result = calloc.allocate<UpdateResult>(8);
          result.ref.status = -1; // invalid status code
          addTearDown(() => calloc.free(result));
          when(() => updater.downloadUpdateWithError()).thenReturn(result);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('throws $UpdateException', () async {
          await expectLater(
            shorebirdUpdater.update,
            throwsA(
              isA<UpdateException>().having(
                (e) => e.message,
                'message',
                'An unknown error occurred.',
              ),
            ),
          );
          verify(updater.downloadUpdateWithError).called(1);
        });
      });

      group('when download succeeds', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          final result = calloc.allocate<UpdateResult>(8);
          result.ref.status = 1; // SHOREBIRD_UPDATE_SUCCESS
          addTearDown(() => calloc.free(result));
          when(() => updater.downloadUpdateWithError()).thenReturn(result);
          shorebirdUpdater = ShorebirdUpdaterImpl(updater, run: run);
        });

        test('completes', () async {
          await expectLater(shorebirdUpdater.update(), completes);
          verify(updater.downloadUpdateWithError).called(1);
        });
      });
    });
  });
}
