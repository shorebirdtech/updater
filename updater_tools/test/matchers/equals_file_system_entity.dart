import 'dart:io';

import 'package:test/test.dart';

EqualsFileSystemEntity equalsFileSystemEntity(FileSystemEntity expected) =>
    EqualsFileSystemEntity(expected);

/// {@template equals_file_system_entity}
/// Matches a [FileSystemEntity] that has the same type and path as [expected].
/// {@endtemplate}
class EqualsFileSystemEntity extends Matcher {
  /// {@macro equals_file_system_entity}
  EqualsFileSystemEntity(this.expected);

  /// The [FileSystemEntity] to compare against.
  final FileSystemEntity expected;

  @override
  bool matches(Object? actual, Map<dynamic, dynamic> matchState) {
    return actual is FileSystemEntity &&
        actual.runtimeType == expected.runtimeType &&
        actual.path == expected.path;
  }

  @override
  Description describeMismatch(
    dynamic actual,
    Description mismatchDescription,
    Map<dynamic, dynamic> matchState,
    bool verbose,
  ) {
    if (actual is! FileSystemEntity) {
      mismatchDescription.add('is not a FileSystemEntity');
    } else if (actual.runtimeType != expected.runtimeType) {
      mismatchDescription.add('is not a ${expected.runtimeType}');
    } else if (actual.path != expected.path) {
      mismatchDescription.add(
        'does not have path ${expected.path} (actual ${actual.path})',
      );
    }

    return mismatchDescription;
  }

  @override
  Description describe(Description description) {
    return description.add(
      '''is a FileSystemEntity of type ${expected.runtimeType} and path ${expected.path}''',
    );
  }
}
