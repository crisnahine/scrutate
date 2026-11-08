# Puppet sample
# Installs nginx.
class nginx (
  String $version = 'latest',
) {
  package { 'nginx': ensure => $version }
}
define nginx::vhost (String $server) {
  file { "/etc/nginx/${server}": ensure => file }
}
node 'web.example.com' {
  include nginx
}
