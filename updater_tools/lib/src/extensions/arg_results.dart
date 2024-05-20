import 'dart:io';

import 'package:args/args.dart';

/// Extension methods for validating options provided to [ArgResults].
extension ArgResultsValidation on ArgResults {
  /// Returns the value of the option with the given [name] as a [File].
  ///
  /// Throws an [ArgumentError] if the option is not present or if the file
  /// does not exist.
  File asExistingFile(String name) {
    final file = File(this[name] as String);
    if (!file.existsSync()) {
      throw ArgumentError.value(
        file.path,
        name,
        'The $name file does not exist',
      );
    }

    return file;
  }
}
