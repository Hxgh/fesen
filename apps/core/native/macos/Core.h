#include <stddef.h>
#include <stdint.h>

typedef void (*ph_callback)(uintptr_t, const uint8_t *, size_t);
typedef int32_t (*ph_adjust)(uintptr_t, const uint8_t *, size_t);
typedef void (*ph_expired)(uintptr_t);

void *ph_core_start(const char *, uintptr_t, ph_adjust, ph_callback, ph_expired);
void ph_core_packet(void *, const uint8_t *, size_t);
void ph_core_call(void *, const uint8_t *, size_t, uintptr_t, ph_callback);
void ph_core_changed(void *);
void ph_core_stop(void *);

void ph_publisher_run(void);
void ph_publisher_changed(void);
void ph_publisher_stop(void);
