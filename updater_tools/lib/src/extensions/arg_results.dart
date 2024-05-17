import 'dart:io';

import 'package:args/args.dart';

/// Extension methods for validating options provided to [ArgResults].
extension ArgResultsValidation on ArgResults {
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
