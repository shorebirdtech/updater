import 'dart:io';

import 'package:args/args.dart';

/// Extension methods for validating options provided to [ArgResults].
extension ArgResultsValidation on ArgResults {
  /// Returns the File at the path specified by [name] if it exists. An
  /// [ArgumentError] is thrown if the file does not exist.
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

  /// Returns the File at the path specified by [name] if [name] was parsed. If
  /// [name] was not parsed, `null` is returned. An [ArgumentError] is thrown if
  /// the file does not exist.
  File? asExistingFileIfParsed(String name) {
    if (!wasParsed(name)) {
      return null;
    }
    return asExistingFile(name);
  }
}
