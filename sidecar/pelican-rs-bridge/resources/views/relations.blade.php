<x-filament-panels::page>
    @foreach ($this->managers() as $manager)
        @livewire($manager, ['ownerRecord' => $this->ownerRecord(), 'pageClass' => $this->pageClass()], key($manager))
    @endforeach
</x-filament-panels::page>
