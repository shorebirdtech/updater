import 'package:shorebird_code_push/shorebird_code_push.dart';
import 'package:test/test.dart';

import '../override_print.dart';

void main() {
  group(ShorebirdUpdater, () {
    test(
      'can be instantiated',
      overridePrint((_) {
        expect(ShorebirdUpdater.new, returnsNormally);
      }),
    );

    group(UpdateTrack, () {
      test('wraps the track name it is given', () {
        // The predefined tracks are const references, so constructing a
        // custom track — the usage the class documents — is what actually
        // exercises the constructor at runtime.
        const names = ['staging', 'beta', 'stable', 'my_custom_track'];
        for (final name in names) {
          final track = UpdateTrack(name);
          expect(track.value, equals(name));
          expect(track.name, equals(name));
        }
      });
    });

    group(PatchTransition, () {
      test('overrides toString', () {
        const transition = PatchTransition(
          current: Patch(number: 1),
          next: Patch(number: 2),
        );
        expect(
          transition.toString(),
          equals('PatchTransition(current: 1, next: 2)'),
        );
      });

      test('reports a base release as null on both sides', () {
        const transition = PatchTransition(current: null, next: null);
        expect(
          transition.toString(),
          equals('PatchTransition(current: null, next: null)'),
        );
      });

      test('hashes by patch number, so equal transitions collide', () {
        // The stream dedupes by equality, so a hashCode that disagreed with
        // == would let the same pending restart through twice.
        const transition = PatchTransition(
          current: Patch(number: 1),
          next: Patch(number: 2),
        );
        const same = PatchTransition(
          current: Patch(number: 1),
          next: Patch(number: 2),
        );
        const withdrawal =
            PatchTransition(current: Patch(number: 1), next: null);

        expect(transition.hashCode, equals(same.hashCode));
        expect(transition.hashCode, isNot(equals(withdrawal.hashCode)));
      });
    });

    group(UpdateException, () {
      test('overrides toString', () {
        const message = 'message';
        const reason = UpdateFailureReason.downloadFailed;
        const exception = UpdateException(message: message, reason: reason);
        expect(
          exception.toString(),
          equals(
            '[ShorebirdUpdater] UpdateException: $message (${reason.name})',
          ),
        );
      });
    });

    group(ReadPatchException, () {
      test('overrides toString', () {
        const message = 'message';
        const exception = ReadPatchException(message: message);
        expect(
          exception.toString(),
          equals('[ShorebirdUpdater] ReadPatchException: $message'),
        );
      });
    });
  });
}
