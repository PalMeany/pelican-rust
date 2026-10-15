<x-filament-panels::page>
    <x-filament::section>
        <x-filament::link href="/server/{{ \Filament\Facades\Filament::getTenant()?->uuid_short }}" icon="tabler-arrow-left">
            {{ trans('server/console.title') }}
        </x-filament::link>
    </x-filament::section>
</x-filament-panels::page>
