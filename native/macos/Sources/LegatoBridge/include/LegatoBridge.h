#include <stdbool.h>
bool legato_start(void);
char *legato_snapshot(void);
char *legato_command(const char *json);
void legato_free(char *json);
void legato_shutdown(void);
