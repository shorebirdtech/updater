// ignore_for_file: prefer_const_constructors

import 'dart:async';

import 'package:shorebird_code_push/src/shorebird_code_push_noop.dart';
import 'package:test/test.dart';

void main() {
  group(ShorebirdCodePushNoop, () {
    late List<String> printLogs;
    late ShorebirdCodePushNoop shorebirdCodePush;

    setUp(() {
      printLogs = [];
      shorebirdCodePush = runZoned(
        ShorebirdCodePushNoop.new,
        zoneSpecification: ZoneSpecification(
          print: (self, parent, zone, line) => printLogs.add(line),
        ),
      );
    });

    test('logs warning when instantiated', () {
      const expected = '''
ShorebirdCodePush: Shorebird Engine not available.
Using no-op implementation. All methods will return null or false.''';
      expect(printLogs, equals([expected]));
    });

    group('isNewPatchAvailableForDownload', () {
      test('returns false', () {
        expectLater(
          shorebirdCodePush.isNewPatchAvailableForDownload(),
          completion(isFalse),
        );
      });
    });

    group('currentPatchNumber', () {
      test('returns null', () {
        expectLater(shorebirdCodePush.currentPatchNumber(), completion(isNull));
      });
    });

    group('nextPatchNumber', () {
      test('returns null', () {
        expectLater(shorebirdCodePush.nextPatchNumber(), completion(isNull));
      });
    });

    group('downloadUpdate', () {
      test('completes', () {
        expectLater(shorebirdCodePush.downloadUpdateIfAvailable(), completes);
      });
    });

    group('isNewPatchReadyToInstall', () {
      test('returns false', () {
        expectLater(
          shorebirdCodePush.isNewPatchReadyToInstall(),
          completion(isFalse),
        );
      });
    });
  });
}
