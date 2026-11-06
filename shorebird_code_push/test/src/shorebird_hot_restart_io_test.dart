import 'dart:async';

import 'package:mocktail/mocktail.dart';
import 'package:shorebird_code_push/src/shorebird_hot_restart_io.dart';
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
  group(ShorebirdHotRestartImpl, () {
    late Updater updater;
    late ShorebirdUpdater shorebirdUpdater;
    late ShorebirdHotRestartImpl hotRestart;

    // The same mock stands in for the FFI wrapper and for what the updater
    // reads, so a test stubs patch numbers and restart acceptance in one
    // place.
    void build(IsolateRun isolateRun) {
      shorebirdUpdater = ShorebirdUpdaterImpl(
        updater: updater,
        run: isolateRun,
      );
      hotRestart = ShorebirdHotRestartImpl(
        updater: shorebirdUpdater,
        ffi: updater,
      );
    }

    setUp(() {
      updater = _MockUpdater();
    });

    group('dangerouslyHotRestart', () {
      group('when updater is unavailable', () {
        setUp(() {
          when(updater.currentPatchNumber).thenThrow(Exception('oops'));
        });

        test(
          'returns false without calling the updater',
          overridePrint((_) async {
            build(run);
            await expectLater(
              hotRestart.dangerouslyHotRestart(),
              completion(isFalse),
            );
            verifyNever(updater.restartApp);
          }),
        );
      });

      group('when the engine accepts the restart request', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          when(updater.restartApp).thenReturn(true);
          build(run);
        });

        test('returns true', () async {
          await expectLater(
            hotRestart.dangerouslyHotRestart(),
            completion(isTrue),
          );
          verify(updater.restartApp).called(1);
        });
      });

      group('when the engine rejects the restart request', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          when(updater.restartApp).thenReturn(false);
          build(run);
        });

        test('returns false', () async {
          await expectLater(
            hotRestart.dangerouslyHotRestart(),
            completion(isFalse),
          );
          verify(updater.restartApp).called(1);
        });
      });

      group('when the engine does not support hot restart', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          // Engines that predate hot restart support don't export the
          // symbol. The late-bound FFI lookup throws an ArgumentError.
          when(updater.restartApp).thenThrow(
            ArgumentError('Failed to lookup symbol shorebird_restart_app'),
          );
          build(run);
        });

        test('returns false', () async {
          await expectLater(
            hotRestart.dangerouslyHotRestart(),
            completion(isFalse),
          );
          verify(updater.restartApp).called(1);
        });
      });
    });
  });
}
