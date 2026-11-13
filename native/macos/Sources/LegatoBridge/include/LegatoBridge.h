// The C ABI of crates/legato-swift-bridge. Every returned string is owned JSON:
// free it exactly once with legato_free.
#include <stdbool.h>

bool legato_start(void);
void legato_set_observer(void (*observer)(void));
char *legato_snapshot(void);
char *legato_command(const char *json);
void legato_free(char *json);
void legato_shutdown(void);
