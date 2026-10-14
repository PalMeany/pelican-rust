<?php
// Dump every translation group (app + vendor namespaces) per locale as JSON.
require '/var/www/html/vendor/autoload.php';
$app = require '/var/www/html/bootstrap/app.php';
$app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();
$loader = app('translator')->getLoader();
$groups = function (string $dir, string $locale): array {
    $base = "$dir/$locale";
    if (!is_dir($base)) return [];
    $out = [];
    $it = new RecursiveIteratorIterator(new RecursiveDirectoryIterator($base, FilesystemIterator::SKIP_DOTS));
    foreach ($it as $f) {
        if ($f->getExtension() !== 'php') continue;
        $rel = substr($f->getPathname(), strlen($base) + 1, -4);
        $out[] = str_replace(DIRECTORY_SEPARATOR, '/', $rel);
    }
    sort($out);
    return $out;
};
$locales = array_values(array_filter(scandir(lang_path()), fn ($d) => $d[0] !== '.' && is_dir(lang_path($d)) && $d !== 'vendor'));
$result = [];
foreach ($locales as $locale) {
    $data = [];
    // App groups plus the framework's own (passwords, auth, pagination, validation fallbacks).
    $framework = base_path('vendor/laravel/framework/src/Illuminate/Translation/lang');
    foreach (array_unique(array_merge($groups(lang_path(), $locale), $groups($framework, $locale))) as $g) {
        $data[$g] = $loader->load($locale, $g, '*');
    }
    foreach ($loader->namespaces() as $ns => $path) {
        foreach (array_unique(array_merge($groups($path, $locale), $groups(lang_path("vendor/$ns"), $locale))) as $g) {
            $lines = $loader->load($locale, $g, $ns);
            if ($lines) $data["$ns::$g"] = $lines;
        }
    }
    $json = $loader->load($locale, '*', '*');
    if ($json) $data['*'] = $json;
    $result[$locale] = $data;
}
echo json_encode($result, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
