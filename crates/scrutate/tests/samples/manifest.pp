# Sample Puppet manifest.
class webserver (
  String $docroot = '/var/www',
) {
  package { 'nginx':
    ensure => installed,
  }
}

define webserver::vhost (
  String $server_name,
) {
  file { "/etc/nginx/sites/${server_name}":
    ensure => file,
  }
}
