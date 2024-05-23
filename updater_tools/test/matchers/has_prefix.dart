import 'package:collection/collection.dart';
import 'package:test/test.dart';

HasPrefix<T> hasPrefix<T>(Iterable<T> expected) => HasPrefix<T>(expected);

/// {@template has_prefix}
/// Matches an [Iterable] that starts with the expected elements.
/// {@endtemplate}
class HasPrefix<T> extends TypeMatcher<Iterable<T>> {
  /// {@macro has_prefix}
  HasPrefix(this.expected);

  final Iterable<T> expected;

  @override
  bool matches(dynamic actual, Map<dynamic, dynamic> matchState) {
    return actual is Iterable<T> &&
        actual.length >= expected.length &&
        IterableZip([actual, expected]).every(
          (pair) => pair.first == pair.last,
        );
  }

  @override
  Description describe(Description description) {
    return description.add('starts with $expected');
  }
}
