<?php

namespace PelicanRs\Bridge;

use App\Models\Plugin;
use Closure;
use Illuminate\Console\Scheduling\CallbackEvent;
use Illuminate\Console\Scheduling\Event as SchedulingEvent;
use Illuminate\Console\Scheduling\Schedule;
use ReflectionFunction;
use ReflectionProperty;

/** Which plugin a class, closure or listener comes from. */
final class Origin
{
    /** @var array<string, string>|null namespace prefix => plugin id */
    private static ?array $namespaces = null;

    /** @return array<string, string> */
    public static function namespaces(): array
    {
        if (is_null(self::$namespaces)) {
            self::$namespaces = [];
            foreach (Plugin::query()->get() as $plugin) {
                if ($plugin->id !== 'pelican-rs-bridge' && $plugin->shouldLoad()) {
                    self::$namespaces[trim($plugin->namespace, '\\') . '\\'] = $plugin->id;
                }
            }
        }

        return self::$namespaces;
    }

    public static function pluginOfClass(?string $class): ?string
    {
        $class = ltrim((string) $class, '\\');
        foreach (self::namespaces() as $prefix => $id) {
            if (str_starts_with($class, $prefix)) {
                return $id;
            }
        }

        return null;
    }

    public static function pluginOfFile(?string $file): ?string
    {
        $file = (string) $file;
        $base = rtrim(base_path('plugins'), '/') . '/';
        if (!str_starts_with($file, $base)) {
            return null;
        }
        $id = strtok(substr($file, strlen($base)), '/');

        return in_array($id, self::namespaces(), true) ? $id : null;
    }

    /** A listener as registered with the dispatcher (class, "Class@method", [obj, method] or closure). */
    public static function pluginOfListener(mixed $listener): ?string
    {
        if ($listener instanceof Closure) {
            $reflection = new ReflectionFunction($listener);
            $bound = $reflection->getClosureScopeClass()?->getName();

            return self::pluginOfClass($bound) ?? self::pluginOfFile($reflection->getFileName());
        }
        if (is_array($listener)) {
            return self::pluginOfClass(is_object($listener[0]) ? $listener[0]::class : $listener[0]);
        }
        if (is_string($listener)) {
            return self::pluginOfClass(explode('@', $listener)[0]);
        }

        return null;
    }

    /** Artisan command names defined by plugins. */
    public static function pluginCommands(): array
    {
        $names = [];
        foreach (Plugin::query()->get() as $plugin) {
            if (!isset(self::namespaces()[trim($plugin->namespace, '\\') . '\\'])) {
                continue;
            }
            foreach ($plugin->getCommands() as $class) {
                if (class_exists($class)) {
                    $names[] = app($class)->getName();
                }
            }
        }

        return array_filter($names);
    }

    public static function pluginOfScheduledEvent(SchedulingEvent $event): ?string
    {
        if ($event instanceof CallbackEvent) {
            $callback = (new ReflectionProperty(CallbackEvent::class, 'callback'))->getValue($event);
            if ($id = is_callable($callback) ? self::pluginOfListener($callback) : null) {
                return $id;
            }

            // `$schedule->job(new Job)` describes itself with the job class.
            return self::pluginOfClass($event->description);
        }
        foreach (self::pluginCommands() as $name) {
            if (preg_match('/(^|\s|\')' . preg_quote($name, '/') . '(\'|\s|$)/', (string) $event->command)) {
                return 'command';
            }
        }

        return null;
    }

    public static function keepPluginSchedule(Schedule $schedule): void
    {
        $events = new ReflectionProperty(Schedule::class, 'events');
        $events->setValue($schedule, array_values(array_filter(
            $schedule->events(),
            fn (SchedulingEvent $event) => !is_null(self::pluginOfScheduledEvent($event)),
        )));
    }
}
