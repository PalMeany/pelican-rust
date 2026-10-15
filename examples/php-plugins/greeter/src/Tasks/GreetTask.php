<?php

namespace Example\Greeter\Tasks;

use App\Extensions\Tasks\TaskSchemaInterface;
use App\Models\Schedule;
use App\Models\Task;

class GreetTask implements TaskSchemaInterface
{
    public function getId(): string
    {
        return 'greeter-greet';
    }

    public function getName(): string
    {
        return trans('greeter::greeter.task');
    }

    public function runTask(Task $task): void
    {
        cache()->forever('greeter.last_task', $task->payload . ' @ ' . now()->toIso8601String());
    }

    public function canCreate(Schedule $schedule): bool
    {
        return true;
    }

    public function getDefaultPayload(): ?string
    {
        return 'hello';
    }

    public function getPayloadLabel(): ?string
    {
        return 'Greeting';
    }

    public function formatPayload(string $payload): null|string|array
    {
        return $payload;
    }

    public function getPayloadForm(): array
    {
        return [];
    }
}
