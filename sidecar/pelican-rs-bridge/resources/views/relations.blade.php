<x-filament-panels::page>
    {{-- One tab panel per manager; pelican-rs reads data-rs-relation for the tab buttons. --}}
    @foreach ($this->managers() as $manager)
        <div data-tab-panel="rel-plugin-{{ $loop->index }}" data-rs-relation="{{ base64_encode(json_encode($this->tab($manager))) }}" @if (! $loop->first) hidden @endif>
            @livewire($manager, ['ownerRecord' => $this->ownerRecord(), 'pageClass' => $this->pageClass()], key($manager))
        </div>
    @endforeach
</x-filament-panels::page>
