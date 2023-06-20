#ifndef updater_h
#define updater_h

/* Warning, this file is autogenerated by cbindgen. Don't modify this manually. */

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#ifdef _WIN32
#define SHOREBIRD_EXPORT __declspec(dllexport)
#else
#define SHOREBIRD_EXPORT __attribute__((visibility("default")))
#endif


/**
 * Struct containing configuration parameters for the updater.
 * Passed to all updater functions.
 * NOTE: If this struct is changed all language bindings must be updated.
 */
typedef struct AppParameters {
  /**
   * release_version, required.  Named version of the app, off of which
   * updates are based.  Can be either a version number or a hash.
   */
  const char *release_version;
  /**
   * Array of paths to the original aot library, required.  For Flutter apps
   * these are the paths to the bundled libapp.so.  May be used for
   * compression downloaded artifacts.
   */
  const char *const *original_libapp_paths;
  /**
   * Length of the original_libapp_paths array.
   */
  int original_libapp_paths_size;
  /**
   * Path to cache_dir where the updater will store downloaded artifacts.
   */
  const char *cache_dir;
} AppParameters;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Configures updater.  First parameter is a struct containing configuration
 * from the running app.  Second parameter is a YAML string containing
 * configuration compiled into the app.  Returns true on success and false on
 * failure. If false is returned, the updater library will not be usable.
 */
SHOREBIRD_EXPORT
bool shorebird_init(const struct AppParameters *c_params,
                    const char *c_yaml);

/**
 * The patch number that will boot on the next run of the app, or 0 if there is
 * no next patch.
 */
SHOREBIRD_EXPORT uintptr_t shorebird_next_boot_patch_number(void);

/**
 * The path to the patch that will boot on the next run of the app, or NULL if
 * there is no next patch.
 */
SHOREBIRD_EXPORT char *shorebird_next_boot_patch_path(void);

/**
 * Free a string returned by the updater library.
 */
SHOREBIRD_EXPORT void shorebird_free_string(char *c_string);

/**
 * Check for an update.  Returns true if an update is available.
 */
SHOREBIRD_EXPORT bool shorebird_check_for_update(void);

/**
 * Synchronously download an update if one is available.
 */
SHOREBIRD_EXPORT void shorebird_update(void);

/**
 * Start a thread to download an update if one is available.
 */
SHOREBIRD_EXPORT void shorebird_start_update_thread(void);

/**
 * Tell the updater that we're launching from what it told us was the
 * next patch to boot from.  This will copy the next_boot patch to be
 * the current_boot patch.
 * It is required to call this function before calling
 * shorebird_report_launch_success or shorebird_report_launch_failure.
 */
SHOREBIRD_EXPORT void shorebird_report_launch_start(void);

/**
 * Report that the app failed to launch.  This will cause the updater to
 * attempt to roll back to the previous version if this version has not
 * been launched successfully before.
 */
SHOREBIRD_EXPORT void shorebird_report_launch_failure(void);

/**
 * Report that the app launched successfully.  This will mark the current
 * as having been launched successfully.  We don't currently do anything
 * with this information, but it could be used to record a point at which
 * we will not roll back from.
 * This is not currently wired up to be called from the Engine.  It's unclear
 * where best to connect it.  Expo waits 5 seconds after the app launches
 * and then marks the launch as successful.  We could do something similar.
 */
SHOREBIRD_EXPORT void shorebird_report_launch_success(void);

#ifdef __cplusplus
} // extern "C"
#endif // __cplusplus

#endif /* updater_h */
