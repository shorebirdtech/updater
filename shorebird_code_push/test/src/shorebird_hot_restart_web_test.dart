import 'package:shorebird_code_push/src/shorebird_hot_restart_web.dart';
import 'package:shorebird_code_push/src/shorebird_updater_web.dart';
import 'package:test/test.dart';

import '../override_print.dart';

void main() {
  group(ShorebirdHotRestartImpl, () {
    late ShorebirdHotRestartImpl hotRestart;

    setUp(
      overridePrint((_) {
        hotRestart = ShorebirdHotRestartImpl(updater: ShorebirdUpdaterImpl());
      }),
    );

    group('patchTransitions', () {
      test('emits nothing', () async {
        await expectLater(hotRestart.patchTransitions, emitsDone);
      });
    });

    group('dangerouslyHotRestart', () {
      test('returns false', () async {
        await expectLater(
          hotRestart.dangerouslyHotRestart(),
          completion(isFalse),
        );
      });
    });
  });
}
