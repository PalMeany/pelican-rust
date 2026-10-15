<x-filament-panels::page>
    <x-filament::section>
        <p>Hello, {{ $user }}! Servers: {{ $servers }}. Last server update seen: {{ $last ?? '—' }}.</p>
        <x-filament::button wire:click="click">Clicked {{ $clicks }} times</x-filament::button>
    </x-filament::section>
</x-filament-panels::page>
