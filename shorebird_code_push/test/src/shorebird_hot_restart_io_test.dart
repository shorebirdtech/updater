import 'dart:async';
import 'dart:ffi';

import 'package:ffi/ffi.dart';
import 'package:mocktail/mocktail.dart';
import 'package:shorebird_code_push/src/generated/updater_bindings.g.dart';
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

    setUpAll(() {
      registerFallbackValue(Pointer.fromAddress(0));
    });

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

    group('patchTransitions', () {
      /// Collects everything the stream emits, so a test can assert on the
      /// absence of an event as well as its presence.
      List<PatchTransition> collect(Stream<PatchTransition> stream) {
        final events = <PatchTransition>[];
        final subscription = stream.listen(events.add);
        addTearDown(subscription.cancel);
        return events;
      }

      group('when updater is unavailable', () {
        setUp(() {
          when(updater.currentPatchNumber).thenThrow(Exception('oops'));
        });

        test(
          'emits nothing',
          overridePrint((_) async {
            build(run);
            final events = collect(hotRestart.patchTransitions);
            await pumpEventQueue();
            expect(events, isEmpty);
          }),
        );
      });

      group('when a patch is already waiting', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(1);
          when(updater.nextPatchNumber).thenReturn(2);
          build(run);
        });

        test('tells a new listener about it', () async {
          final events = collect(hotRestart.patchTransitions);
          await pumpEventQueue();
          expect(events, [
            const PatchTransition(
              current: Patch(number: 1),
              next: Patch(number: 2),
            ),
          ]);
        });

        test('emits once however many times the transition is read', () async {
          when(() => updater.checkForDownloadableUpdate()).thenReturn(false);
          final events = collect(hotRestart.patchTransitions);
          await shorebirdUpdater.checkForUpdate();
          await shorebirdUpdater.checkForUpdate();
          await pumpEventQueue();
          expect(events, hasLength(1));
        });

        test('tells every listener', () async {
          final first = collect(hotRestart.patchTransitions);
          final second = collect(hotRestart.patchTransitions);
          await pumpEventQueue();
          expect(first, hasLength(1));
          expect(second, equals(first));
        });

        test('an install while it waits does not emit twice', () async {
          final result = calloc.allocate<UpdateResult>(sizeOf<UpdateResult>());
          result.ref.status = SHOREBIRD_UPDATE_INSTALLED;
          addTearDown(() => calloc.free(result));
          when(() => updater.update()).thenReturn(result);

          final events = collect(hotRestart.patchTransitions);
          await shorebirdUpdater.update();
          await pumpEventQueue();
          expect(events, hasLength(1));

          // A different patch is a different transition, so it does emit.
          when(updater.nextPatchNumber).thenReturn(3);
          await shorebirdUpdater.update();
          await pumpEventQueue();
          expect(events, hasLength(2));
        });

        test('a cancelled listener hears nothing more', () async {
          final events = <PatchTransition>[];
          await hotRestart.patchTransitions.listen(events.add).cancel();

          when(() => updater.checkForDownloadableUpdate()).thenReturn(false);
          await shorebirdUpdater.checkForUpdate();
          await pumpEventQueue();
          expect(events, isEmpty);
        });

        test('a failed read on subscribe leaves the listener alive', () async {
          when(updater.nextPatchNumber).thenThrow(Exception('oops'));
          final events = collect(hotRestart.patchTransitions);
          await pumpEventQueue();
          expect(events, isEmpty);

          // The catch-up swallowed the failure rather than tearing the
          // subscription down, so a later live event still arrives.
          when(updater.nextPatchNumber).thenReturn(2);
          when(() => updater.checkForDownloadableUpdate()).thenReturn(false);
          await shorebirdUpdater.checkForUpdate();
          await pumpEventQueue();
          expect(events, [
            const PatchTransition(
              current: Patch(number: 1),
              next: Patch(number: 2),
            ),
          ]);
        });
      });

      group('when nothing is waiting', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(1);
          when(updater.nextPatchNumber).thenReturn(1);
          when(() => updater.checkForDownloadableUpdate()).thenReturn(false);
          build(run);
        });

        test('emits nothing', () async {
          final events = collect(hotRestart.patchTransitions);
          await shorebirdUpdater.checkForUpdate();
          await pumpEventQueue();
          expect(events, isEmpty);
        });

        test('emits when checkForUpdate finds the patch withdrawn', () async {
          final events = collect(hotRestart.patchTransitions);
          await pumpEventQueue();
          expect(events, isEmpty);

          // The server pulled the running patch, so next_boot_patch is gone
          // and a restart drops the app back to the base release.
          when(updater.nextPatchNumber).thenReturn(0);
          await shorebirdUpdater.checkForUpdate();
          await pumpEventQueue();
          expect(events, [
            const PatchTransition(current: Patch(number: 1), next: null),
          ]);
        });
      });

      group('when the catch-up read resolves after a live event', () {
        // Each patch read is its own isolate spawn, so the read a listener
        // starts with can land after an install that was published while it
        // was still in flight. The read captures its value up front and
        // resolves late, which is the order that used to deliver a stale
        // patch as the newest one.
        var readLatency = Duration.zero;
        Future<R> latentRun<R>(
          FutureOr<R> Function() computation, {
          String? debugName,
        }) async {
          // Captured on the way in, so a read already in flight keeps the
          // latency it started with.
          final latency = readLatency;
          final value = await computation();
          await Future<void>.delayed(latency);
          return value;
        }

        setUp(() {
          readLatency = Duration.zero;
          when(updater.currentPatchNumber).thenReturn(1);
          when(updater.nextPatchNumber).thenReturn(2);
          final result = calloc.allocate<UpdateResult>(sizeOf<UpdateResult>());
          result.ref.status = SHOREBIRD_UPDATE_INSTALLED;
          addTearDown(() => calloc.free(result));
          when(() => updater.update()).thenReturn(result);
          build(latentRun);
        });

        test('the live event is not overwritten by the stale read', () async {
          readLatency = const Duration(milliseconds: 50);
          final events = collect(hotRestart.patchTransitions);

          readLatency = Duration.zero;
          when(updater.nextPatchNumber).thenReturn(3);
          await shorebirdUpdater.update();
          await pumpEventQueue();
          expect(events, [
            const PatchTransition(
              current: Patch(number: 1),
              next: Patch(number: 3),
            ),
          ]);

          // Long enough for the catch-up read, which captured next=2, to
          // resolve.
          await Future<void>.delayed(const Duration(milliseconds: 100));
          expect(events, hasLength(1));
        });
      });

      group('when update installs a patch', () {
        setUp(() {
          when(updater.currentPatchNumber).thenReturn(0);
          when(updater.nextPatchNumber).thenReturn(0);
          final result = calloc.allocate<UpdateResult>(sizeOf<UpdateResult>());
          result.ref.status = SHOREBIRD_UPDATE_INSTALLED;
          addTearDown(() => calloc.free(result));
          when(() => updater.update()).thenReturn(result);
          build(run);
        });

        test('emits the installed patch', () async {
          final events = collect(hotRestart.patchTransitions);
          await pumpEventQueue();
          expect(events, isEmpty);

          when(updater.nextPatchNumber).thenReturn(3);
          await shorebirdUpdater.update();
          await pumpEventQueue();
          expect(events, [
            const PatchTransition(current: null, next: Patch(number: 3)),
          ]);
        });

        test('a failed read leaves the successful install alone', () async {
          final events = collect(hotRestart.patchTransitions);
          await pumpEventQueue();

          when(updater.nextPatchNumber).thenThrow(Exception('oops'));
          await expectLater(shorebirdUpdater.update(), completes);
          await pumpEventQueue();
          expect(events, isEmpty);
        });
      });
    });
  });
}
