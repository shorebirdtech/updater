import 'dart:async';

import 'package:shorebird_code_push/shorebird_code_push_io.dart';
import 'package:test/test.dart';

void main() {
  group(ShorebirdCodePush, () {
    test('logs error when updater cannot be initialized', () {
      final printLogs = <String>[];
      runZoned(
        ShorebirdCodePush.new,
        zoneSpecification: ZoneSpecification(
          print: (self, parent, zone, line) => printLogs.add(line),
        ),
      );
      expect(
        printLogs,
        [
          startsWith(
            '''[ShorebirdCodePush]: Error initializing updater: Invalid argument(s): Failed to lookup symbol''',
          ),
          equals(
            '''[ShorebirdCodePush]: Shorebird Engine not available, using no-op implementation.\n''',
          ),
        ],
      );
    });
  });
}
