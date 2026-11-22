// The C ABI of crates/legato-swift-bridge. Every returned string is owned JSON:
// free it exactly once with legato_free.
#include <stdbool.h>
#include <stdint.h>

bool legato_start(void);
void legato_set_observer(void (*observer)(void));
char *legato_snapshot(void);
char *legato_command(const char *json);
char *legato_mac_key(uint16_t keycode);
char *legato_key_targets(void);
void legato_free(char *json);
void legato_shutdown(void);
